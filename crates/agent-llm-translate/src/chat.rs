//! Responses API ⇄ OpenAI Chat Completions.

use serde_json::{Map, Value, json};

use crate::{
    TranslateError,
    request::{self, InputItem, Part, ToolChoice},
    response,
};

pub fn responses_to_chat(request: &Value, model: &str) -> Result<Value, TranslateError> {
    request::ensure_supported(request)?;

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        messages.push(json!({"role": "system", "content": instructions}));
    }

    for item in request::parse_input(request)? {
        match item {
            InputItem::Message { role, parts } => {
                let role = match role.as_str() {
                    "system" | "developer" => "system",
                    "assistant" => "assistant",
                    _ => "user",
                };
                messages.push(json!({"role": role, "content": chat_content(&parts)}));
            }
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let tool_call = json!({
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                });
                // Merge consecutive function_call items into one assistant
                // message so strict servers accept parallel call layouts
                // (assistant [A, B] → tool A → tool B).
                if let Some(calls) = messages
                    .last_mut()
                    .filter(|last| last.get("role").and_then(Value::as_str) == Some("assistant"))
                    .and_then(|last| last.get_mut("tool_calls"))
                    .and_then(Value::as_array_mut)
                {
                    calls.push(tool_call);
                } else {
                    messages.push(json!({
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [tool_call],
                    }));
                }
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(messages));

    let tools: Vec<Value> = request::parse_tools(request)
        .into_iter()
        .map(|tool| {
            let mut function = Map::new();
            function.insert("name".into(), json!(tool.name));
            if let Some(description) = tool.description {
                function.insert("description".into(), json!(description));
            }
            if let Some(parameters) = tool.parameters {
                function.insert("parameters".into(), parameters);
            }
            json!({"type": "function", "function": function})
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }

    if let Some(choice) = request::parse_tool_choice(request) {
        let mapped = match choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Required => json!("required"),
            ToolChoice::None => json!("none"),
            ToolChoice::Function(name) => {
                json!({"type": "function", "function": {"name": name}})
            }
        };
        out.insert("tool_choice".into(), mapped);
    }

    for key in ["temperature", "top_p"] {
        if let Some(value) = request.get(key).filter(|value| value.is_number()) {
            out.insert(key.into(), value.clone());
        }
    }

    if let Some(max_tokens) = request.get("max_output_tokens").and_then(Value::as_u64) {
        // Older OpenAI-compatible servers (LM Studio among them) only honor
        // max_tokens; newer ones prefer max_completion_tokens. Send both.
        out.insert("max_tokens".into(), json!(max_tokens));
        out.insert("max_completion_tokens".into(), json!(max_tokens));
    }

    if let Some(effort) = request::reasoning_effort(request) {
        out.insert("reasoning_effort".into(), json!(effort));
    }

    if let Some(parallel) = request.get("parallel_tool_calls").and_then(Value::as_bool) {
        out.insert("parallel_tool_calls".into(), json!(parallel));
    }

    if let Some(format) = request.pointer("/text/format")
        && format.get("type").and_then(Value::as_str) == Some("json_schema")
    {
        let mut json_schema = Map::new();
        if let Some(name) = format.get("name") {
            json_schema.insert("name".into(), name.clone());
        }
        if let Some(schema) = format.get("schema") {
            json_schema.insert("schema".into(), schema.clone());
        }
        if let Some(strict) = format.get("strict") {
            json_schema.insert("strict".into(), strict.clone());
        }
        out.insert(
            "response_format".into(),
            json!({"type": "json_schema", "json_schema": json_schema}),
        );
    }

    if request::wants_stream(request) {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }

    Ok(Value::Object(out))
}

fn chat_content(parts: &[Part]) -> Value {
    match parts {
        [] => json!(""),
        [Part::Text(text)] => json!(text),
        parts => Value::Array(
            parts
                .iter()
                .map(|part| match part {
                    Part::Text(text) => json!({"type": "text", "text": text}),
                    Part::Image(url) => json!({"type": "image_url", "image_url": {"url": url}}),
                })
                .collect(),
        ),
    }
}

pub fn chat_response_to_responses(
    upstream: &Value,
    requested_model: &str,
    response_id: &str,
    created_at: i64,
) -> Result<Value, TranslateError> {
    let choice = upstream
        .pointer("/choices/0")
        .ok_or_else(|| TranslateError::new("upstream chat response has no choices"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| TranslateError::new("upstream chat response has no message"))?;

    let mut output = Vec::new();
    if let Some(reasoning) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(response::reasoning_item(
            &format!("rs_{}", output.len()),
            reasoning,
        ));
    }
    if let Some(content) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(response::message_item(
            &format!("msg_{}", output.len()),
            content,
            "completed",
        ));
    }
    for tool_call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        output.push(response::function_call_item(
            &format!("fc_{}", output.len()),
            tool_call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            tool_call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "completed",
        ));
    }

    let incomplete = choice.get("finish_reason").and_then(Value::as_str) == Some("length");
    let usage = chat_usage(upstream.get("usage"));
    Ok(response::assemble(
        response_id,
        requested_model,
        created_at,
        output,
        usage,
        incomplete,
    ))
}

fn chat_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.filter(|usage| usage.is_object()) else {
        return Value::Null;
    };
    let read = |key: &str| usage.get(key).and_then(Value::as_i64);
    response::usage_value(
        read("prompt_tokens"),
        read("completion_tokens"),
        read("total_tokens"),
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_instructions_and_string_input() {
        let out = responses_to_chat(
            &json!({"instructions": "Be terse.", "input": "hello"}),
            "qwen3.6-35b-a3b",
        )
        .expect("translates");
        assert_eq!(out["model"], "qwen3.6-35b-a3b");
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "Be terse.");
        assert_eq!(out["messages"][1]["role"], "user");
        assert_eq!(out["messages"][1]["content"], "hello");
        assert!(out.get("stream").is_none());
    }

    #[test]
    fn maps_multipart_content_with_images() {
        let out = responses_to_chat(
            &json!({"input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "what is this?"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
                ]
            }]}),
            "m",
        )
        .expect("translates");
        let content = &out["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn developer_role_becomes_system() {
        let out = responses_to_chat(
            &json!({"input": [{"type": "message", "role": "developer", "content": "rule"}]}),
            "m",
        )
        .expect("translates");
        assert_eq!(out["messages"][0]["role"], "system");
    }

    #[test]
    fn maps_function_call_round_trip_shapes() {
        let out = responses_to_chat(
            &json!({"input": [
                {"type": "function_call", "call_id": "c1", "name": "ls", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "README.md"}
            ]}),
            "m",
        )
        .expect("translates");
        let assistant = &out["messages"][0];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["tool_calls"][0]["id"], "c1");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "ls");
        let tool = &out["messages"][1];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "c1");
        assert_eq!(tool["content"], "README.md");
    }

    #[test]
    fn merges_consecutive_function_calls_into_one_assistant_message() {
        let out = responses_to_chat(
            &json!({"input": [
                {"type": "function_call", "call_id": "a", "name": "f", "arguments": "{}"},
                {"type": "function_call", "call_id": "b", "name": "g", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "a", "output": "1"},
                {"type": "function_call_output", "call_id": "b", "output": "2"}
            ]}),
            "m",
        )
        .expect("translates");
        let messages = out["messages"].as_array().expect("array");
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0]["tool_calls"].as_array().expect("calls").len(),
            2
        );
    }

    #[test]
    fn maps_tools_and_tool_choice_variants() {
        let request = json!({
            "tools": [{"type": "function", "name": "get_weather", "description": "d", "parameters": {"type": "object"}}],
            "tool_choice": "required",
        });
        let out = responses_to_chat(&request, "m").expect("translates");
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(out["tool_choice"], "required");

        let out = responses_to_chat(
            &json!({"tool_choice": {"type": "function", "name": "get_weather"}}),
            "m",
        )
        .expect("translates");
        assert_eq!(out["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn maps_sampling_limits_reasoning_and_stream() {
        let out = responses_to_chat(
            &json!({
                "temperature": 0.2,
                "top_p": 0.9,
                "max_output_tokens": 512,
                "reasoning": {"effort": "high"},
                "parallel_tool_calls": false,
                "stream": true,
            }),
            "m",
        )
        .expect("translates");
        assert_eq!(out["temperature"], 0.2);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["max_tokens"], 512);
        assert_eq!(out["max_completion_tokens"], 512);
        assert_eq!(out["reasoning_effort"], "high");
        assert_eq!(out["parallel_tool_calls"], false);
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);
    }

    #[test]
    fn maps_json_schema_text_format() {
        let out = responses_to_chat(
            &json!({"text": {"format": {"type": "json_schema", "name": "result", "schema": {"type": "object"}, "strict": true}}}),
            "m",
        )
        .expect("translates");
        assert_eq!(out["response_format"]["type"], "json_schema");
        assert_eq!(out["response_format"]["json_schema"]["name"], "result");
        assert_eq!(out["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn rejects_previous_response_id() {
        let error = responses_to_chat(&json!({"previous_response_id": "resp_0"}), "m")
            .expect_err("must reject");
        assert!(error.message.contains("previous_response_id"));
    }

    #[test]
    fn translates_text_only_chat_response() {
        let response = chat_response_to_responses(
            &json!({
                "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 2, "total_tokens": 9},
            }),
            "lmstudio/qwen",
            "resp_1",
            1234,
        )
        .expect("translates");
        assert_eq!(response["id"], "resp_1");
        assert_eq!(response["status"], "completed");
        assert_eq!(response["model"], "lmstudio/qwen");
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(response["output"][0]["content"][0]["text"], "hi");
        assert_eq!(response["usage"]["input_tokens"], 7);
        assert_eq!(response["usage"]["total_tokens"], 9);
    }

    #[test]
    fn translates_reasoning_and_tool_calls() {
        let response = chat_response_to_responses(
            &json!({
                "choices": [{"message": {
                    "reasoning_content": "thinking...",
                    "content": "done",
                    "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}}]
                }, "finish_reason": "tool_calls"}],
            }),
            "kimi/k3",
            "resp_2",
            1,
        )
        .expect("translates");
        let output = response["output"].as_array().expect("output");
        assert_eq!(output.len(), 3);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "thinking...");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(output[2]["call_id"], "call_1");
        assert_eq!(output[2]["arguments"], "{\"path\":\".\"}");
        assert!(response["usage"].is_null());
    }

    #[test]
    fn maps_length_finish_to_incomplete() {
        let response = chat_response_to_responses(
            &json!({"choices": [{"message": {"content": "cut"}, "finish_reason": "length"}]}),
            "m",
            "resp_3",
            1,
        )
        .expect("translates");
        assert_eq!(response["status"], "incomplete");
        assert_eq!(
            response["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn maps_cached_and_reasoning_token_details() {
        let response = chat_response_to_responses(
            &json!({
                "choices": [{"message": {"content": "x"}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150,
                    "prompt_tokens_details": {"cached_tokens": 80},
                    "completion_tokens_details": {"reasoning_tokens": 20}
                },
            }),
            "m",
            "resp_4",
            1,
        )
        .expect("translates");
        assert_eq!(
            response["usage"]["input_tokens_details"]["cached_tokens"],
            80
        );
        assert_eq!(
            response["usage"]["output_tokens_details"]["reasoning_tokens"],
            20
        );
    }

    #[test]
    fn errors_on_missing_choices() {
        assert!(chat_response_to_responses(&json!({}), "m", "r", 1).is_err());
    }
}
