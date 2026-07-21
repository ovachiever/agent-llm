//! Anthropic Messages ⇄ OpenAI Chat Completions — the reverse direction.
//!
//! This is what lets an Anthropic-dialect client (Claude Code) drive a
//! Chat-Completions upstream (OpenRouter, LM Studio, OpenAI). Requests are
//! translated forward, responses and SSE streams are translated back into
//! Anthropic shapes.

use serde_json::{Map, Value, json};

use crate::{SseEvent, TranslateError, stream::OutEvent};

// ---------------------------------------------------------------------------
// Request: Anthropic Messages -> Chat Completions
// ---------------------------------------------------------------------------

pub fn anthropic_to_chat(request: &Value, model: &str) -> Result<Value, TranslateError> {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = system_text(request.get("system"))
        && !system.is_empty()
    {
        messages.push(json!({"role": "system", "content": system}));
    }

    for message in request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match message.get("content") {
            Some(Value::String(text)) => {
                messages.push(json!({"role": role, "content": text}));
            }
            Some(Value::Array(blocks)) => match role {
                "assistant" => push_assistant_blocks(blocks, &mut messages),
                _ => push_user_blocks(blocks, &mut messages),
            },
            _ => {}
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(messages));

    let tools: Vec<Value> = request
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tool| {
            // Server tools (web_search_*, computer_* and friends) carry a
            // `type` field; a foreign upstream cannot execute them.
            match tool.get("type").and_then(Value::as_str) {
                None | Some("custom") => true,
                Some(_) => false,
            }
        })
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            let mut function = Map::new();
            function.insert("name".into(), json!(name));
            if let Some(description) = tool.get("description").and_then(Value::as_str) {
                function.insert("description".into(), json!(description));
            }
            if let Some(schema) = tool.get("input_schema") {
                function.insert("parameters".into(), schema.clone());
            }
            Some(json!({"type": "function", "function": function}))
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }

    if let Some(choice) = request.get("tool_choice") {
        let mapped = match choice.get("type").and_then(Value::as_str) {
            Some("auto") => Some(json!("auto")),
            Some("any") => Some(json!("required")),
            Some("none") => Some(json!("none")),
            Some("tool") => choice
                .get("name")
                .and_then(Value::as_str)
                .map(|name| json!({"type": "function", "function": {"name": name}})),
            _ => None,
        };
        if let Some(mapped) = mapped {
            out.insert("tool_choice".into(), mapped);
        }
    }

    if let Some(max_tokens) = request.get("max_tokens").and_then(Value::as_u64) {
        // Older OpenAI-compatible servers only honor max_tokens; newer ones
        // prefer max_completion_tokens. Send both.
        out.insert("max_tokens".into(), json!(max_tokens));
        out.insert("max_completion_tokens".into(), json!(max_tokens));
    }

    for key in ["temperature", "top_p"] {
        if let Some(value) = request.get(key).filter(|value| value.is_number()) {
            out.insert(key.into(), value.clone());
        }
    }

    if let Some(stop) = request.get("stop_sequences").filter(|stop| stop.is_array()) {
        out.insert("stop".into(), stop.clone());
    }

    if let Some(effort) = reasoning_effort(request) {
        out.insert("reasoning_effort".into(), json!(effort));
    }

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }

    Ok(Value::Object(out))
}

fn system_text(system: Option<&Value>) -> Option<String> {
    match system? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        _ => None,
    }
}

fn push_assistant_blocks(blocks: &[Value], messages: &mut Vec<Value>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(fragment) = block.get("text").and_then(Value::as_str) {
                    text.push_str(fragment);
                }
            }
            Some("tool_use") => {
                let arguments = block
                    .get("input")
                    .map(|input| serde_json::to_string(input).unwrap_or_else(|_| "{}".into()))
                    .unwrap_or_else(|| "{}".into());
                tool_calls.push(json!({
                    "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "type": "function",
                    "function": {
                        "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "arguments": arguments,
                    },
                }));
            }
            // thinking / redacted_thinking: a foreign upstream cannot verify them.
            _ => {}
        }
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    if message["content"].is_null() && !message.contains_key("tool_calls") {
        return;
    }
    messages.push(Value::Object(message));
}

fn push_user_blocks(blocks: &[Value], messages: &mut Vec<Value>) {
    let mut parts: Vec<Value> = Vec::new();
    let mut has_image = false;

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_result") => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),
                    "content": tool_result_text(block.get("content")),
                }));
            }
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({"type": "text", "text": text}));
                }
            }
            Some("image") => {
                if let Some(url) = image_url(block.get("source")) {
                    has_image = true;
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
            }
            // document / thinking / unknown blocks: dropped.
            _ => {}
        }
    }

    if parts.is_empty() {
        return;
    }
    let content = if !has_image && parts.len() == 1 {
        parts[0]["text"].clone()
    } else {
        Value::Array(parts)
    };
    messages.push(json!({"role": "user", "content": content}));
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| match block.get("text").and_then(Value::as_str) {
                Some(text) if block.get("type").and_then(Value::as_str) == Some("text") => {
                    text.to_string()
                }
                _ => serde_json::to_string(block).unwrap_or_default(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
        None => String::new(),
    }
}

fn image_url(source: Option<&Value>) -> Option<String> {
    let source = source?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            Some(format!("data:{media_type};base64,{data}"))
        }
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn reasoning_effort(request: &Value) -> Option<&'static str> {
    if let Some(effort) = request
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
    {
        return Some(match effort {
            "minimal" => "minimal",
            "low" => "low",
            "medium" => "medium",
            _ => "high",
        });
    }
    let thinking = request.get("thinking")?;
    if thinking.get("type").and_then(Value::as_str) != Some("enabled") {
        return None;
    }
    let budget = thinking
        .get("budget_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    Some(match budget {
        ..4096 => "low",
        4096..16384 => "medium",
        _ => "high",
    })
}

// ---------------------------------------------------------------------------
// Response: Chat Completions -> Anthropic Messages
// ---------------------------------------------------------------------------

pub fn chat_response_to_anthropic(
    upstream: &Value,
    requested_model: &str,
    response_id: &str,
) -> Result<Value, TranslateError> {
    let choice = upstream
        .pointer("/choices/0")
        .ok_or_else(|| TranslateError::new("upstream chat response has no choices"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| TranslateError::new("upstream chat response has no message"))?;

    let mut content = Vec::new();
    if let Some(thinking) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type": "thinking", "thinking": thinking, "signature": ""}));
    }
    if let Some(text) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type": "text", "text": text}));
    }
    for tool_call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let input = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
            .unwrap_or_else(|| json!({}));
        content.push(json!({
            "type": "tool_use",
            "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name": tool_call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default(),
            "input": input,
        }));
    }

    let stop_reason = map_stop_reason(choice.get("finish_reason").and_then(Value::as_str));
    Ok(json!({
        "id": response_id,
        "type": "message",
        "role": "assistant",
        "model": requested_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": anthropic_usage(upstream.get("usage")),
    }))
}

fn map_stop_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

fn anthropic_usage(usage: Option<&Value>) -> Value {
    let read = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    json!({
        "input_tokens": read("prompt_tokens"),
        "output_tokens": read("completion_tokens"),
        "cache_read_input_tokens": usage
            .and_then(|usage| usage.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        "cache_creation_input_tokens": 0,
    })
}

// ---------------------------------------------------------------------------
// Streaming: Chat Completions SSE -> Anthropic Messages SSE
// ---------------------------------------------------------------------------

enum ReverseBlock {
    Thinking,
    Text,
    ToolUse {
        chat_index: i64,
        opened: bool,
        call_id: String,
        name: String,
        buffered: Vec<String>,
    },
}

pub struct ReverseStreamTranslator {
    response_id: String,
    model: String,
    started: bool,
    finished: bool,
    next_index: i64,
    block: Option<ReverseBlock>,
    stop_reason: Option<&'static str>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

impl ReverseStreamTranslator {
    pub fn new(requested_model: &str, response_id: &str) -> Self {
        Self {
            response_id: response_id.to_string(),
            model: requested_model.to_string(),
            started: false,
            finished: false,
            next_index: 0,
            block: None,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
        }
    }

    pub fn push_event(&mut self, event: &SseEvent) -> Vec<OutEvent> {
        if self.finished {
            return Vec::new();
        }
        let mut events = Vec::new();
        let data = event.data.trim();
        if data == "[DONE]" {
            self.ensure_started(&mut events);
            self.complete(&mut events);
            return events;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return events;
        };
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            self.fail(error, &mut events);
            return events;
        }
        self.ensure_started(&mut events);

        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            let read = |key: &str| usage.get(key).and_then(Value::as_i64);
            self.input_tokens = read("prompt_tokens").or(self.input_tokens);
            self.output_tokens = read("completion_tokens").or(self.output_tokens);
            self.total_tokens = read("total_tokens").or(self.total_tokens);
        }

        let Some(choice) = chunk.pointer("/choices/0") else {
            return events;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            // Recorded here, emitted at [DONE]/finish(): the usage-bearing
            // chunk arrives after finish_reason on OpenAI-compatible streams.
            self.stop_reason = Some(map_stop_reason(Some(reason)));
        }
        let Some(delta) = choice.get("delta") else {
            return events;
        };

        if let Some(text) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.thinking_delta(text, &mut events);
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.text_delta(text, &mut events);
        }
        for tool_call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.tool_call_delta(tool_call, &mut events);
        }
        events
    }

    /// EOF flush: close the response if the upstream ended without `[DONE]`.
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

    // ---- ladder plumbing ----

    fn ensure_started(&mut self, events: &mut Vec<OutEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        self.emit(
            events,
            "message_start",
            json!({"message": {
                "id": self.response_id,
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }}),
        );
    }

    fn thinking_delta(&mut self, text: &str, events: &mut Vec<OutEvent>) {
        if !matches!(self.block, Some(ReverseBlock::Thinking)) {
            self.close_block(events);
            self.emit(
                events,
                "content_block_start",
                json!({
                    "index": self.next_index,
                    "content_block": {"type": "thinking", "thinking": "", "signature": ""},
                }),
            );
            self.block = Some(ReverseBlock::Thinking);
        }
        self.emit(
            events,
            "content_block_delta",
            json!({
                "index": self.next_index,
                "delta": {"type": "thinking_delta", "thinking": text},
            }),
        );
    }

    fn text_delta(&mut self, text: &str, events: &mut Vec<OutEvent>) {
        if !matches!(self.block, Some(ReverseBlock::Text)) {
            self.close_block(events);
            self.emit(
                events,
                "content_block_start",
                json!({
                    "index": self.next_index,
                    "content_block": {"type": "text", "text": ""},
                }),
            );
            self.block = Some(ReverseBlock::Text);
        }
        self.emit(
            events,
            "content_block_delta",
            json!({
                "index": self.next_index,
                "delta": {"type": "text_delta", "text": text},
            }),
        );
    }

    fn tool_call_delta(&mut self, tool_call: &Value, events: &mut Vec<OutEvent>) {
        let index = tool_call.get("index").and_then(Value::as_i64).unwrap_or(0);
        let same_call = matches!(
            &self.block,
            Some(ReverseBlock::ToolUse { chat_index, .. }) if *chat_index == index
        );
        if !same_call {
            self.close_block(events);
            self.block = Some(ReverseBlock::ToolUse {
                chat_index: index,
                opened: false,
                call_id: String::new(),
                name: String::new(),
                buffered: Vec::new(),
            });
        }

        let fragment = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .filter(|fragment| !fragment.is_empty())
            .map(ToOwned::to_owned);
        let delta_id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let delta_name = tool_call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());

        let index_for_emit = self.next_index;
        let Some(ReverseBlock::ToolUse {
            opened,
            call_id,
            name,
            buffered,
            ..
        }) = &mut self.block
        else {
            return;
        };
        if let Some(id) = delta_id {
            *call_id = id.to_string();
        }
        if let Some(new_name) = delta_name {
            *name = new_name.to_string();
        }

        let mut to_emit = Vec::new();
        if !*opened && (!call_id.is_empty() || !name.is_empty()) {
            *opened = true;
            to_emit.push((
                "content_block_start",
                json!({
                    "index": index_for_emit,
                    "content_block": {
                        "type": "tool_use",
                        "id": call_id.clone(),
                        "name": name.clone(),
                        "input": {},
                    },
                }),
            ));
            for buffered_fragment in buffered.drain(..) {
                to_emit.push((
                    "content_block_delta",
                    json!({
                        "index": index_for_emit,
                        "delta": {"type": "input_json_delta", "partial_json": buffered_fragment},
                    }),
                ));
            }
        }
        if let Some(fragment) = fragment {
            if *opened {
                to_emit.push((
                    "content_block_delta",
                    json!({
                        "index": index_for_emit,
                        "delta": {"type": "input_json_delta", "partial_json": fragment},
                    }),
                ));
            } else {
                buffered.push(fragment);
            }
        }
        for (event, data) in to_emit {
            self.emit(events, event, data);
        }
    }

    fn close_block(&mut self, events: &mut Vec<OutEvent>) {
        let Some(block) = self.block.take() else {
            return;
        };
        match block {
            ReverseBlock::Thinking => {
                // Harness compatibility: a thinking block carries a signature
                // delta before it closes.
                self.emit(
                    events,
                    "content_block_delta",
                    json!({
                        "index": self.next_index,
                        "delta": {"type": "signature_delta", "signature": ""},
                    }),
                );
            }
            ReverseBlock::ToolUse {
                opened: false,
                call_id,
                name,
                buffered,
                chat_index,
            } => {
                // The upstream never named the call; synthesize so the ladder
                // stays well-formed.
                self.emit(
                    events,
                    "content_block_start",
                    json!({
                        "index": self.next_index,
                        "content_block": {
                            "type": "tool_use",
                            "id": if call_id.is_empty() { format!("call_{chat_index}") } else { call_id },
                            "name": name,
                            "input": {},
                        },
                    }),
                );
                for fragment in buffered {
                    self.emit(
                        events,
                        "content_block_delta",
                        json!({
                            "index": self.next_index,
                            "delta": {"type": "input_json_delta", "partial_json": fragment},
                        }),
                    );
                }
            }
            _ => {}
        }
        self.emit(
            events,
            "content_block_stop",
            json!({"index": self.next_index}),
        );
        self.next_index += 1;
    }

    fn complete(&mut self, events: &mut Vec<OutEvent>) {
        self.close_block(events);
        self.emit(
            events,
            "message_delta",
            json!({
                "delta": {
                    "stop_reason": self.stop_reason.unwrap_or("end_turn"),
                    "stop_sequence": null,
                },
                "usage": {
                    "input_tokens": self.input_tokens.unwrap_or(0),
                    "output_tokens": self.output_tokens.unwrap_or(0),
                },
            }),
        );
        self.emit(events, "message_stop", json!({}));
        self.finished = true;
    }

    fn fail(&mut self, error: &Value, events: &mut Vec<OutEvent>) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream error")
            .to_string();
        self.emit(
            events,
            "error",
            json!({"error": {"type": "api_error", "message": message}}),
        );
        self.finished = true;
    }

    fn emit(&self, events: &mut Vec<OutEvent>, name: &str, mut data: Value) {
        if let Some(object) = data.as_object_mut() {
            object.insert("type".into(), json!(name));
        }
        events.push(OutEvent {
            event: name.to_string(),
            data,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chunk(data: Value) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_string(),
        }
    }

    fn done() -> SseEvent {
        SseEvent {
            event: None,
            data: "[DONE]".into(),
        }
    }

    fn event_names(events: &[OutEvent]) -> Vec<&str> {
        events.iter().map(|event| event.event.as_str()).collect()
    }

    // ---- request mapping ----

    #[test]
    fn maps_string_system_and_simple_messages() {
        let out = anthropic_to_chat(
            &json!({
                "system": "Be terse.",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 512,
            }),
            "kimi-k3",
        )
        .expect("translates");
        assert_eq!(out["model"], "kimi-k3");
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "Be terse.");
        assert_eq!(out["messages"][1]["content"], "hello");
        assert_eq!(out["max_tokens"], 512);
        assert_eq!(out["max_completion_tokens"], 512);
    }

    #[test]
    fn joins_system_block_arrays() {
        let out = anthropic_to_chat(
            &json!({
                "system": [
                    {"type": "text", "text": "One.", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "Two."}
                ],
                "messages": [],
            }),
            "m",
        )
        .expect("translates");
        assert_eq!(out["messages"][0]["content"], "One.\n\nTwo.");
    }

    #[test]
    fn single_text_block_collapses_to_string_content() {
        let out = anthropic_to_chat(
            &json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]}),
            "m",
        )
        .expect("translates");
        assert_eq!(out["messages"][0]["content"], "hi");
    }

    #[test]
    fn maps_images_to_data_urls_and_plain_urls() {
        let out = anthropic_to_chat(
            &json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "AAAA"}},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/x.png"}}
            ]}]}),
            "m",
        )
        .expect("translates");
        let content = &out["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
        assert_eq!(content[2]["image_url"]["url"], "https://example.com/x.png");
    }

    #[test]
    fn maps_tool_use_and_tool_result_round_trip() {
        let out = anthropic_to_chat(
            &json!({"messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_1", "name": "ls", "input": {"path": "."}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "README.md"}
                ]}
            ]}),
            "m",
        )
        .expect("translates");
        let assistant = &out["messages"][0];
        assert_eq!(assistant["content"], "Let me check.");
        assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\".\"}"
        );
        let tool = &out["messages"][1];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "toolu_1");
        assert_eq!(tool["content"], "README.md");
    }

    #[test]
    fn tool_results_precede_remaining_user_text() {
        let out = anthropic_to_chat(
            &json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "also consider this"},
                {"type": "tool_result", "tool_use_id": "a", "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}]},
                {"type": "tool_result", "tool_use_id": "b", "content": [{"type": "json", "data": {"n": 1}}]}
            ]}]}),
            "m",
        )
        .expect("translates");
        let messages = out["messages"].as_array().expect("array");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["content"], "one\ntwo");
        assert_eq!(messages[1]["role"], "tool");
        assert!(
            messages[1]["content"]
                .as_str()
                .expect("string")
                .contains("\"n\":1")
        );
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "also consider this");
    }

    #[test]
    fn drops_thinking_blocks_and_server_tools() {
        let out = anthropic_to_chat(
            &json!({
                "messages": [{"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "text", "text": "answer"}
                ]}],
                "tools": [
                    {"name": "real_tool", "input_schema": {"type": "object"}},
                    {"type": "web_search_20250305", "name": "web_search"},
                    {"type": "custom", "name": "custom_tool", "input_schema": {"type": "object"}}
                ],
            }),
            "m",
        )
        .expect("translates");
        assert_eq!(out["messages"][0]["content"], "answer");
        let tools = out["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "real_tool");
        assert_eq!(tools[1]["function"]["name"], "custom_tool");
    }

    #[test]
    fn maps_tool_choice_variants() {
        for (anthropic, expected) in [
            (json!({"type": "auto"}), json!("auto")),
            (json!({"type": "any"}), json!("required")),
            (json!({"type": "none"}), json!("none")),
            (
                json!({"type": "tool", "name": "ls"}),
                json!({"type": "function", "function": {"name": "ls"}}),
            ),
        ] {
            let out = anthropic_to_chat(&json!({"messages": [], "tool_choice": anthropic}), "m")
                .expect("translates");
            assert_eq!(out["tool_choice"], expected);
        }
    }

    #[test]
    fn maps_thinking_budget_to_effort_tiers() {
        for (budget, effort) in [(1024, "low"), (8192, "medium"), (32768, "high")] {
            let out = anthropic_to_chat(
                &json!({"messages": [], "thinking": {"type": "enabled", "budget_tokens": budget}}),
                "m",
            )
            .expect("translates");
            assert_eq!(out["reasoning_effort"], effort, "budget {budget}");
        }
        let out = anthropic_to_chat(
            &json!({"messages": [], "thinking": {"type": "adaptive"}}),
            "m",
        )
        .expect("translates");
        assert!(out.get("reasoning_effort").is_none());
    }

    #[test]
    fn output_config_effort_takes_precedence_and_clamps() {
        let out = anthropic_to_chat(
            &json!({
                "messages": [],
                "thinking": {"type": "enabled", "budget_tokens": 1024},
                "output_config": {"effort": "max"},
            }),
            "m",
        )
        .expect("translates");
        assert_eq!(out["reasoning_effort"], "high");
    }

    #[test]
    fn maps_sampling_stops_and_stream() {
        let out = anthropic_to_chat(
            &json!({
                "messages": [],
                "temperature": 0.5,
                "top_p": 0.9,
                "stop_sequences": ["END"],
                "stream": true,
                "metadata": {"user_id": "u"},
            }),
            "m",
        )
        .expect("translates");
        assert_eq!(out["temperature"], 0.5);
        assert_eq!(out["stop"][0], "END");
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);
        assert!(out.get("metadata").is_none());
    }

    // ---- non-stream response mapping ----

    #[test]
    fn translates_text_response_with_usage() {
        let out = chat_response_to_anthropic(
            &json!({
                "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 9, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 4}},
            }),
            "openrouter/moonshotai/kimi-k3",
            "msg_1",
        )
        .expect("translates");
        assert_eq!(out["id"], "msg_1");
        assert_eq!(out["type"], "message");
        assert_eq!(out["model"], "openrouter/moonshotai/kimi-k3");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "hi");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 9);
        assert_eq!(out["usage"]["output_tokens"], 2);
        assert_eq!(out["usage"]["cache_read_input_tokens"], 4);
    }

    #[test]
    fn translates_reasoning_and_tool_calls_to_blocks() {
        let out = chat_response_to_anthropic(
            &json!({"choices": [{"message": {
                "reasoning_content": "let me think",
                "content": "done",
                "tool_calls": [
                    {"id": "call_1", "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}},
                    {"id": "call_2", "function": {"name": "cat", "arguments": "not json"}}
                ]
            }, "finish_reason": "tool_calls"}]}),
            "m",
            "msg_2",
        )
        .expect("translates");
        let content = out["content"].as_array().expect("content");
        assert_eq!(content.len(), 4);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "let me think");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["input"]["path"], ".");
        assert_eq!(content[3]["input"], json!({}));
        assert_eq!(out["stop_reason"], "tool_use");
    }

    #[test]
    fn maps_finish_reasons() {
        for (finish, stop) in [
            ("stop", "end_turn"),
            ("length", "max_tokens"),
            ("tool_calls", "tool_use"),
            ("content_filter", "refusal"),
            ("weird", "end_turn"),
        ] {
            let out = chat_response_to_anthropic(
                &json!({"choices": [{"message": {"content": "x"}, "finish_reason": finish}]}),
                "m",
                "msg",
            )
            .expect("translates");
            assert_eq!(out["stop_reason"], stop, "finish_reason {finish}");
        }
    }

    #[test]
    fn zeroes_missing_usage() {
        let out = chat_response_to_anthropic(
            &json!({"choices": [{"message": {"content": "x"}}]}),
            "m",
            "msg",
        )
        .expect("translates");
        assert_eq!(out["usage"]["input_tokens"], 0);
        assert_eq!(out["usage"]["output_tokens"], 0);
    }

    #[test]
    fn errors_on_missing_choices() {
        assert!(chat_response_to_anthropic(&json!({}), "m", "msg").is_err());
    }

    // ---- streaming ----

    #[test]
    fn streams_text_only_ladder() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_1");
        let mut events = Vec::new();
        events.extend(translator.push_event(&chunk(
            json!({"choices": [{"delta": {"role": "assistant", "content": "Hel"}}]}),
        )));
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"content": "lo"}}]}))),
        );
        events.extend(translator.push_event(&chunk(
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        )));
        events.extend(translator.push_event(&chunk(
            json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}}),
        )));
        events.extend(translator.push_event(&done()));

        assert_eq!(
            event_names(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[1].data["content_block"]["type"], "text");
        assert_eq!(events[2].data["delta"]["text"], "Hel");
        let message_delta = &events[5].data;
        assert_eq!(message_delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(message_delta["usage"]["input_tokens"], 5);
        assert_eq!(message_delta["usage"]["output_tokens"], 2);
        assert_eq!(translator.usage(), (Some(5), Some(2), Some(7)));
    }

    #[test]
    fn streams_thinking_text_and_two_tool_calls_with_indexes() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_2");
        let mut events = Vec::new();
        events.extend(translator.push_event(&chunk(
            json!({"choices": [{"delta": {"reasoning_content": "hmm"}}]}),
        )));
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"content": "ok"}}]}))),
        );
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_a", "function": {"name": "ls", "arguments": "{\"p\""}}
            ]}}]}))),
        );
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": ":1}"}}
            ]}}]}))),
        );
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 1, "id": "call_b", "function": {"name": "cat", "arguments": "{}"}}
            ]}}]}))),
        );
        events.extend(translator.push_event(&chunk(
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        )));
        events.extend(translator.push_event(&done()));

        assert_eq!(
            event_names(&events),
            vec![
                "message_start",
                "content_block_start", // thinking, index 0
                "content_block_delta", // thinking_delta
                "content_block_delta", // signature_delta
                "content_block_stop",  // index 0
                "content_block_start", // text, index 1
                "content_block_delta", // text_delta
                "content_block_stop",  // index 1
                "content_block_start", // tool_use call_a, index 2
                "content_block_delta", // input_json_delta {"p"
                "content_block_delta", // input_json_delta :1}
                "content_block_stop",  // index 2
                "content_block_start", // tool_use call_b, index 3
                "content_block_delta", // input_json_delta {}
                "content_block_stop",  // index 3
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[1].data["content_block"]["type"], "thinking");
        assert_eq!(events[3].data["delta"]["type"], "signature_delta");
        assert_eq!(events[8].data["index"], 2);
        assert_eq!(events[8].data["content_block"]["id"], "call_a");
        assert_eq!(events[8].data["content_block"]["name"], "ls");
        assert_eq!(events[12].data["index"], 3);
        assert_eq!(events[12].data["content_block"]["name"], "cat");
        assert_eq!(events[15].data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn buffers_tool_fragments_until_id_or_name_arrives() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_3");
        let mut events = Vec::new();
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"a\""}}
            ]}}]}))),
        );
        assert_eq!(event_names(&events), vec!["message_start"]);
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_x", "function": {"name": "f", "arguments": ":1}"}}
            ]}}]}))),
        );
        events.extend(translator.push_event(&done()));
        let names = event_names(&events);
        assert_eq!(names[1], "content_block_start");
        assert_eq!(names[2], "content_block_delta");
        assert_eq!(names[3], "content_block_delta");
        assert_eq!(events[2].data["delta"]["partial_json"], "{\"a\"");
        assert_eq!(events[3].data["delta"]["partial_json"], ":1}");
    }

    #[test]
    fn finish_without_done_flushes_ladder() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_4");
        let mut events = Vec::new();
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"content": "cut"}}]}))),
        );
        events.extend(translator.finish());
        assert_eq!(
            event_names(&events),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert!(translator.finish().is_empty());
    }

    #[test]
    fn finish_on_empty_stream_emits_minimal_ladder() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_5");
        let events = translator.finish();
        assert_eq!(
            event_names(&events),
            vec!["message_start", "message_delta", "message_stop"]
        );
    }

    #[test]
    fn upstream_error_becomes_anthropic_error_event() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_6");
        let events = translator.push_event(&chunk(json!({"error": {"message": "rate limited"}})));
        assert_eq!(event_names(&events), vec!["error"]);
        assert_eq!(events[0].data["error"]["type"], "api_error");
        assert_eq!(events[0].data["error"]["message"], "rate limited");
        assert!(translator.push_event(&done()).is_empty());
        assert!(translator.finish().is_empty());
    }

    #[test]
    fn length_finish_maps_to_max_tokens_stop() {
        let mut translator = ReverseStreamTranslator::new("m", "msg_7");
        let mut events = Vec::new();
        events.extend(
            translator.push_event(&chunk(json!({"choices": [{"delta": {"content": "x"}}]}))),
        );
        events.extend(translator.push_event(&chunk(
            json!({"choices": [{"delta": {}, "finish_reason": "length"}]}),
        )));
        events.extend(translator.push_event(&done()));
        let message_delta = events
            .iter()
            .find(|event| event.event == "message_delta")
            .expect("message_delta");
        assert_eq!(message_delta.data["delta"]["stop_reason"], "max_tokens");
    }
}
