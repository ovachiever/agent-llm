//! Streaming state machine: upstream SSE events in, Responses API SSE events
//! out. One translator instance per proxied request.

use serde_json::{Value, json};

use crate::{Dialect, SseEvent, response};

#[derive(Debug, Clone)]
pub struct OutEvent {
    pub event: String,
    pub data: Value,
}

enum OpenItem {
    Message {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        text: String,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        /// Chat Completions distinguishes parallel calls by `index`;
        /// Anthropic closes blocks explicitly, so it uses -1.
        chat_index: i64,
    },
    /// An upstream block we cannot represent (e.g. redacted_thinking);
    /// deltas for it are swallowed.
    Skipped,
}

pub struct StreamTranslator {
    dialect: Dialect,
    response_id: String,
    model: String,
    created_at: i64,
    seq: u64,
    started: bool,
    finished: bool,
    incomplete: bool,
    output: Vec<Value>,
    current: Option<OpenItem>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    cached_tokens: i64,
}

impl StreamTranslator {
    pub fn new(
        dialect: Dialect,
        requested_model: &str,
        response_id: &str,
        created_at: i64,
    ) -> Self {
        Self {
            dialect,
            response_id: response_id.to_string(),
            model: requested_model.to_string(),
            created_at,
            seq: 0,
            started: false,
            finished: false,
            incomplete: false,
            output: Vec::new(),
            current: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cached_tokens: 0,
        }
    }

    pub fn push_event(&mut self, event: &SseEvent) -> Vec<OutEvent> {
        if self.finished {
            return Vec::new();
        }
        let mut events = Vec::new();
        match self.dialect {
            Dialect::ChatCompletions => self.push_chat(event, &mut events),
            Dialect::AnthropicMessages => self.push_anthropic(event, &mut events),
        }
        events
    }

    /// EOF flush: emit whatever is needed to close the response if the
    /// upstream ended without a terminator.
    pub fn finish(&mut self) -> Vec<OutEvent> {
        if self.finished {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.ensure_started(&mut events);
        self.complete(&mut events);
        events
    }

    pub fn usage(&self) -> (Option<i64>, Option<i64>, Option<i64>) {
        let total = self.total_tokens.or_else(|| {
            self.input_tokens
                .zip(self.output_tokens)
                .map(|(a, b)| a + b)
        });
        (self.input_tokens, self.output_tokens, total)
    }

    // ---- chat completions input ----

    fn push_chat(&mut self, event: &SseEvent, events: &mut Vec<OutEvent>) {
        let data = event.data.trim();
        if data == "[DONE]" {
            self.ensure_started(events);
            self.complete(events);
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };
        self.ensure_started(events);
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            self.fail(error, events);
            return;
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            self.input_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .or(self.input_tokens);
            self.output_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .or(self.output_tokens);
            self.total_tokens = usage
                .get("total_tokens")
                .and_then(Value::as_i64)
                .or(self.total_tokens);
            self.cached_tokens = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(self.cached_tokens);
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return;
        };
        if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
            self.incomplete = true;
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };
        if let Some(text) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.reasoning_delta(text, events);
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.text_delta(text, events);
        }
        for tool_call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.chat_tool_call_delta(tool_call, events);
        }
    }

    fn chat_tool_call_delta(&mut self, tool_call: &Value, events: &mut Vec<OutEvent>) {
        let index = tool_call.get("index").and_then(Value::as_i64).unwrap_or(0);
        let same_call = matches!(
            &self.current,
            Some(OpenItem::FunctionCall { chat_index, .. }) if *chat_index == index
        );
        if !same_call {
            self.close_current(events);
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("call_{index}"));
            let name = tool_call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.open_function_call(call_id, name, index, events);
        }
        if let Some(fragment) = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .filter(|fragment| !fragment.is_empty())
        {
            self.function_arguments_delta(fragment, events);
        }
    }

    // ---- anthropic messages input ----

    fn push_anthropic(&mut self, event: &SseEvent, events: &mut Vec<OutEvent>) {
        let Ok(data) = serde_json::from_str::<Value>(event.data.trim()) else {
            return;
        };
        let name = event
            .event
            .clone()
            .or_else(|| {
                data.get("type")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default();
        match name.as_str() {
            "message_start" => {
                self.ensure_started(events);
                if let Some(usage) = data.pointer("/message/usage") {
                    self.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .or(self.input_tokens);
                    self.cached_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.cached_tokens);
                }
            }
            "content_block_start" => {
                self.ensure_started(events);
                self.close_current(events);
                match data.pointer("/content_block/type").and_then(Value::as_str) {
                    Some("text") => self.open_message(events),
                    Some("thinking") => self.open_reasoning(events),
                    Some("tool_use") => {
                        let call_id = data
                            .pointer("/content_block/id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = data
                            .pointer("/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.open_function_call(call_id, name, -1, events);
                    }
                    _ => self.current = Some(OpenItem::Skipped),
                }
            }
            "content_block_delta" => match data.pointer("/delta/type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = data
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        && !matches!(self.current, Some(OpenItem::Skipped))
                    {
                        self.text_delta(text, events);
                    }
                }
                Some("thinking_delta") => {
                    if let Some(text) = data
                        .pointer("/delta/thinking")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        && !matches!(self.current, Some(OpenItem::Skipped))
                    {
                        self.reasoning_delta(text, events);
                    }
                }
                Some("input_json_delta") => {
                    if let Some(fragment) = data
                        .pointer("/delta/partial_json")
                        .and_then(Value::as_str)
                        .filter(|fragment| !fragment.is_empty())
                    {
                        self.function_arguments_delta(fragment, events);
                    }
                }
                // signature_delta and unknown delta kinds carry nothing we
                // can forward.
                _ => {}
            },
            "content_block_stop" => self.close_current(events),
            "message_delta" => {
                if data.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("max_tokens")
                {
                    self.incomplete = true;
                }
                if let Some(tokens) = data.pointer("/usage/output_tokens").and_then(Value::as_i64) {
                    self.output_tokens = Some(tokens);
                }
            }
            "message_stop" => {
                self.ensure_started(events);
                self.complete(events);
            }
            "error" => {
                self.ensure_started(events);
                let error = data.get("error").cloned().unwrap_or(data);
                self.fail(&error, events);
            }
            // ping and unknown events.
            _ => {}
        }
    }

    // ---- shared emission ----

    fn event(&mut self, name: &str, mut data: Value) -> OutEvent {
        if let Some(map) = data.as_object_mut() {
            map.insert("type".into(), json!(name));
            map.insert("sequence_number".into(), json!(self.seq));
        }
        self.seq += 1;
        OutEvent {
            event: name.to_string(),
            data,
        }
    }

    fn partial_response(&self) -> Value {
        response::skeleton(
            &self.response_id,
            &self.model,
            self.created_at,
            "in_progress",
        )
    }

    fn ensure_started(&mut self, events: &mut Vec<OutEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let created = json!({"response": self.partial_response()});
        events.push(self.event("response.created", created));
        let in_progress = json!({"response": self.partial_response()});
        events.push(self.event("response.in_progress", in_progress));
    }

    fn open_message(&mut self, events: &mut Vec<OutEvent>) {
        let output_index = self.output.len();
        let id = format!("msg_{output_index}");
        let added = json!({
            "output_index": output_index,
            "item": {"id": id, "type": "message", "role": "assistant", "status": "in_progress", "content": []},
        });
        events.push(self.event("response.output_item.added", added));
        let part = json!({
            "item_id": id,
            "output_index": output_index,
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []},
        });
        events.push(self.event("response.content_part.added", part));
        self.current = Some(OpenItem::Message {
            id,
            text: String::new(),
        });
    }

    fn open_reasoning(&mut self, events: &mut Vec<OutEvent>) {
        let output_index = self.output.len();
        let id = format!("rs_{output_index}");
        let added = json!({
            "output_index": output_index,
            "item": {"id": id, "type": "reasoning", "summary": []},
        });
        events.push(self.event("response.output_item.added", added));
        let part = json!({
            "item_id": id,
            "output_index": output_index,
            "summary_index": 0,
            "part": {"type": "summary_text", "text": ""},
        });
        events.push(self.event("response.reasoning_summary_part.added", part));
        self.current = Some(OpenItem::Reasoning {
            id,
            text: String::new(),
        });
    }

    fn open_function_call(
        &mut self,
        call_id: String,
        name: String,
        chat_index: i64,
        events: &mut Vec<OutEvent>,
    ) {
        let output_index = self.output.len();
        let id = format!("fc_{output_index}");
        let added = json!({
            "output_index": output_index,
            "item": {
                "id": id,
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": "",
                "status": "in_progress",
            },
        });
        events.push(self.event("response.output_item.added", added));
        self.current = Some(OpenItem::FunctionCall {
            id,
            call_id,
            name,
            arguments: String::new(),
            chat_index,
        });
    }

    fn text_delta(&mut self, text: &str, events: &mut Vec<OutEvent>) {
        if !matches!(self.current, Some(OpenItem::Message { .. })) {
            self.close_current(events);
            self.open_message(events);
        }
        let (item_id, output_index) = {
            let Some(OpenItem::Message { id, text: buffer }) = &mut self.current else {
                return;
            };
            buffer.push_str(text);
            (id.clone(), self.output.len())
        };
        let delta = json!({
            "item_id": item_id,
            "output_index": output_index,
            "content_index": 0,
            "delta": text,
        });
        events.push(self.event("response.output_text.delta", delta));
    }

    fn reasoning_delta(&mut self, text: &str, events: &mut Vec<OutEvent>) {
        if !matches!(self.current, Some(OpenItem::Reasoning { .. })) {
            self.close_current(events);
            self.open_reasoning(events);
        }
        let (item_id, output_index) = {
            let Some(OpenItem::Reasoning { id, text: buffer }) = &mut self.current else {
                return;
            };
            buffer.push_str(text);
            (id.clone(), self.output.len())
        };
        let delta = json!({
            "item_id": item_id,
            "output_index": output_index,
            "summary_index": 0,
            "delta": text,
        });
        events.push(self.event("response.reasoning_summary_text.delta", delta));
    }

    fn function_arguments_delta(&mut self, fragment: &str, events: &mut Vec<OutEvent>) {
        let (item_id, output_index) = {
            let Some(OpenItem::FunctionCall { id, arguments, .. }) = &mut self.current else {
                // Arguments cannot be attributed without an open call.
                return;
            };
            arguments.push_str(fragment);
            (id.clone(), self.output.len())
        };
        let delta = json!({
            "item_id": item_id,
            "output_index": output_index,
            "delta": fragment,
        });
        events.push(self.event("response.function_call_arguments.delta", delta));
    }

    fn close_current(&mut self, events: &mut Vec<OutEvent>) {
        let Some(item) = self.current.take() else {
            return;
        };
        let output_index = self.output.len();
        match item {
            OpenItem::Message { id, text } => {
                let done = json!({
                    "item_id": id,
                    "output_index": output_index,
                    "content_index": 0,
                    "text": text,
                });
                events.push(self.event("response.output_text.done", done));
                let part = json!({
                    "item_id": id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": text, "annotations": []},
                });
                events.push(self.event("response.content_part.done", part));
                let item = response::message_item(&id, &text, "completed");
                let done = json!({"output_index": output_index, "item": item});
                events.push(self.event("response.output_item.done", done));
                self.output.push(item);
            }
            OpenItem::Reasoning { id, text } => {
                let done = json!({
                    "item_id": id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "text": text,
                });
                events.push(self.event("response.reasoning_summary_text.done", done));
                let part = json!({
                    "item_id": id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "part": {"type": "summary_text", "text": text},
                });
                events.push(self.event("response.reasoning_summary_part.done", part));
                let item = response::reasoning_item(&id, &text);
                let done = json!({"output_index": output_index, "item": item});
                events.push(self.event("response.output_item.done", done));
                self.output.push(item);
            }
            OpenItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                ..
            } => {
                let done = json!({
                    "item_id": id,
                    "output_index": output_index,
                    "arguments": arguments,
                });
                events.push(self.event("response.function_call_arguments.done", done));
                let item =
                    response::function_call_item(&id, &call_id, &name, &arguments, "completed");
                let done = json!({"output_index": output_index, "item": item});
                events.push(self.event("response.output_item.done", done));
                self.output.push(item);
            }
            OpenItem::Skipped => {}
        }
    }

    fn complete(&mut self, events: &mut Vec<OutEvent>) {
        if self.finished {
            return;
        }
        self.close_current(events);
        self.finished = true;
        let (input, output_tokens, total) = self.usage();
        let usage = response::usage_value(input, output_tokens, total, self.cached_tokens, 0);
        let response = response::assemble(
            &self.response_id,
            &self.model,
            self.created_at,
            self.output.clone(),
            usage,
            self.incomplete,
        );
        let completed = json!({"response": response});
        events.push(self.event("response.completed", completed));
    }

    fn fail(&mut self, error: &Value, events: &mut Vec<OutEvent>) {
        self.close_current(events);
        self.finished = true;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| error.to_string());
        let (input, output_tokens, total) = self.usage();
        let usage = response::usage_value(input, output_tokens, total, self.cached_tokens, 0);
        let mut response = response::assemble(
            &self.response_id,
            &self.model,
            self.created_at,
            self.output.clone(),
            usage,
            false,
        );
        response["status"] = json!("failed");
        response["error"] = json!({"code": "upstream_error", "message": message});
        let failed = json!({"response": response});
        events.push(self.event("response.failed", failed));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data_event(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_string(),
        }
    }

    fn named_event(name: &str, data: Value) -> SseEvent {
        SseEvent {
            event: Some(name.to_string()),
            data: data.to_string(),
        }
    }

    fn names(events: &[OutEvent]) -> Vec<&str> {
        events.iter().map(|event| event.event.as_str()).collect()
    }

    fn chat_translator() -> StreamTranslator {
        StreamTranslator::new(Dialect::ChatCompletions, "lmstudio/qwen", "resp_t", 42)
    }

    fn anthropic_translator() -> StreamTranslator {
        StreamTranslator::new(Dialect::AnthropicMessages, "kimi/k3", "resp_t", 42)
    }

    #[test]
    fn chat_text_stream_emits_full_event_ladder() {
        let mut translator = chat_translator();
        let mut all = Vec::new();
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        )));
        all.extend(
            translator.push_event(&data_event(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#)),
        );
        all.extend(
            translator.push_event(&data_event(r#"{"choices":[{"delta":{"content":"lo"}}]}"#)),
        );
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        )));
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#,
        )));
        all.extend(translator.push_event(&data_event("[DONE]")));

        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let sequence: Vec<u64> = all
            .iter()
            .map(|event| event.data["sequence_number"].as_u64().expect("seq"))
            .collect();
        assert_eq!(sequence, (0..all.len() as u64).collect::<Vec<_>>());

        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["model"], "lmstudio/qwen");
        assert_eq!(completed["output"][0]["content"][0]["text"], "Hello");
        assert_eq!(completed["usage"]["input_tokens"], 10);
        assert_eq!(translator.usage(), (Some(10), Some(2), Some(12)));
    }

    #[test]
    fn chat_reasoning_transitions_to_message() {
        let mut translator = chat_translator();
        let mut all = Vec::new();
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
        )));
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"content":"answer"}}]}"#,
        )));
        all.extend(translator.push_event(&data_event("[DONE]")));

        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done",
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["output"][0]["type"], "reasoning");
        assert_eq!(completed["output"][1]["type"], "message");
    }

    #[test]
    fn chat_parallel_tool_calls_split_by_index() {
        let mut translator = chat_translator();
        let mut all = Vec::new();
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"ls","arguments":"{\"p"}}]}}]}"#,
        )));
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\":1}"}}]}}]}"#,
        )));
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"cat","arguments":"{}"}}]}}]}"#,
        )));
        all.extend(translator.push_event(&data_event("[DONE]")));

        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["output"][0]["call_id"], "call_a");
        assert_eq!(completed["output"][0]["arguments"], "{\"p\":1}");
        assert_eq!(completed["output"][1]["call_id"], "call_b");
        assert_eq!(completed["output"][1]["name"], "cat");
    }

    #[test]
    fn chat_finish_without_done_marker_is_flushed_by_finish() {
        let mut translator = chat_translator();
        let mut all = Vec::new();
        all.extend(translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"content":"partial"}}]}"#,
        )));
        all.extend(translator.finish());
        assert_eq!(all.last().expect("last").event, "response.completed");
        assert!(translator.finish().is_empty());
    }

    #[test]
    fn chat_length_finish_marks_incomplete() {
        let mut translator = chat_translator();
        translator.push_event(&data_event(
            r#"{"choices":[{"delta":{"content":"cut"},"finish_reason":"length"}]}"#,
        ));
        let all = translator.push_event(&data_event("[DONE]"));
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["status"], "incomplete");
        assert_eq!(
            completed["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn chat_error_chunk_fails_the_response_and_swallows_the_rest() {
        let mut translator = chat_translator();
        let all = translator.push_event(&data_event(r#"{"error":{"message":"model not loaded"}}"#));
        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.failed"
            ]
        );
        let failed = &all.last().expect("failed").data["response"];
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"]["message"], "model not loaded");
        assert!(
            translator
                .push_event(&data_event(r#"{"choices":[{"delta":{"content":"x"}}]}"#))
                .is_empty()
        );
        assert!(translator.finish().is_empty());
    }

    #[test]
    fn anthropic_full_ladder_with_thinking_text_and_tool_use() {
        let mut translator = anthropic_translator();
        let mut all = Vec::new();
        all.extend(translator.push_event(&named_event(
            "message_start",
            json!({"message": {"id": "msg_up", "usage": {"input_tokens": 10, "cache_read_input_tokens": 4}}}),
        )));
        all.extend(translator.push_event(&named_event("ping", json!({}))));
        all.extend(translator.push_event(&named_event(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "thinking"}}),
        )));
        all.extend(translator.push_event(&named_event(
            "content_block_delta",
            json!({"delta": {"type": "thinking_delta", "thinking": "hmm"}}),
        )));
        all.extend(translator.push_event(&named_event("content_block_stop", json!({"index": 0}))));
        all.extend(translator.push_event(&named_event(
            "content_block_start",
            json!({"index": 1, "content_block": {"type": "text"}}),
        )));
        all.extend(translator.push_event(&named_event(
            "content_block_delta",
            json!({"delta": {"type": "text_delta", "text": "hi"}}),
        )));
        all.extend(translator.push_event(&named_event("content_block_stop", json!({"index": 1}))));
        all.extend(translator.push_event(&named_event(
            "content_block_start",
            json!({"index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "ls"}}),
        )));
        all.extend(translator.push_event(&named_event(
            "content_block_delta",
            json!({"delta": {"type": "input_json_delta", "partial_json": "{\"path\":\".\"}"}}),
        )));
        all.extend(translator.push_event(&named_event("content_block_stop", json!({"index": 2}))));
        all.extend(translator.push_event(&named_event(
            "message_delta",
            json!({"delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 7}}),
        )));
        all.extend(translator.push_event(&named_event("message_stop", json!({}))));

        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done",
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["output"][2]["call_id"], "toolu_1");
        assert_eq!(completed["output"][2]["arguments"], "{\"path\":\".\"}");
        assert_eq!(completed["usage"]["input_tokens"], 10);
        assert_eq!(completed["usage"]["output_tokens"], 7);
        assert_eq!(completed["usage"]["total_tokens"], 17);
        assert_eq!(
            completed["usage"]["input_tokens_details"]["cached_tokens"],
            4
        );
        assert_eq!(translator.usage(), (Some(10), Some(7), Some(17)));
    }

    #[test]
    fn anthropic_max_tokens_stop_marks_incomplete() {
        let mut translator = anthropic_translator();
        translator.push_event(&named_event("message_start", json!({"message": {}})));
        translator.push_event(&named_event(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "text"}}),
        ));
        translator.push_event(&named_event(
            "content_block_delta",
            json!({"delta": {"type": "text_delta", "text": "cut"}}),
        ));
        translator.push_event(&named_event("content_block_stop", json!({"index": 0})));
        translator.push_event(&named_event(
            "message_delta",
            json!({"delta": {"stop_reason": "max_tokens"}, "usage": {"output_tokens": 3}}),
        ));
        let all = translator.push_event(&named_event("message_stop", json!({})));
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["status"], "incomplete");
    }

    #[test]
    fn anthropic_error_event_fails_the_response() {
        let mut translator = anthropic_translator();
        let all = translator.push_event(&named_event(
            "error",
            json!({"error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ));
        assert_eq!(
            names(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.failed"
            ]
        );
        assert_eq!(
            all.last().expect("failed").data["response"]["error"]["message"],
            "Overloaded"
        );
    }

    #[test]
    fn anthropic_redacted_thinking_block_is_skipped() {
        let mut translator = anthropic_translator();
        translator.push_event(&named_event("message_start", json!({"message": {}})));
        let start = translator.push_event(&named_event(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "redacted_thinking"}}),
        ));
        assert!(start.is_empty());
        let stop = translator.push_event(&named_event("content_block_stop", json!({"index": 0})));
        assert!(stop.is_empty());
        let all = translator.push_event(&named_event("message_stop", json!({})));
        let completed = &all.last().expect("completed").data["response"];
        assert_eq!(completed["output"].as_array().expect("output").len(), 0);
    }

    #[test]
    fn ping_before_message_start_emits_nothing() {
        let mut translator = anthropic_translator();
        assert!(
            translator
                .push_event(&named_event("ping", json!({})))
                .is_empty()
        );
    }
}
