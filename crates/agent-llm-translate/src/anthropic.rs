//! Responses API ⇄ Anthropic Messages.

use serde_json::{Map, Value, json};

use crate::{
    TranslateError, TranslateOptions,
    request::{self, InputItem, Part, ToolChoice},
    response,
};

fn thinking_budget(effort: &str) -> u64 {
    match effort {
        "minimal" => 1_024,
        "low" => 2_048,
        "high" => 16_384,
        "xhigh" => 24_576,
        // "medium" and anything unrecognized.
        _ => 8_192,
    }
}

pub fn responses_to_anthropic(
    request: &Value,
    model: &str,
    opts: &TranslateOptions,
) -> Result<Value, TranslateError> {
    request::ensure_supported(request)?;

    let mut system_parts: Vec<String> = Vec::new();
    if let Some(instructions) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        system_parts.push(instructions.to_string());
    }

    // (role, content blocks); consecutive same-role messages are merged so
    // Anthropic's strict user/assistant alternation holds.
    let mut messages: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push_blocks = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        if let Some((last_role, last_blocks)) = messages.last_mut()
            && last_role == role
        {
            last_blocks.extend(blocks);
        } else {
            messages.push((role.to_string(), blocks));
        }
    };

    for item in request::parse_input(request)? {
        match item {
            InputItem::Message { role, parts } => match role.as_str() {
                "system" | "developer" => {
                    let text = parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text(text) => Some(text.as_str()),
                            Part::Image(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() {
                        system_parts.push(text);
                    }
                }
                role => {
                    let target = if role == "assistant" {
                        "assistant"
                    } else {
                        "user"
                    };
                    let blocks = parts.iter().filter_map(content_block).collect();
                    push_blocks(target, blocks);
                }
            },
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let input: Value = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
                push_blocks(
                    "assistant",
                    vec![json!({"type": "tool_use", "id": call_id, "name": name, "input": input})],
                );
            }
            InputItem::FunctionCallOutput { call_id, output } => {
                push_blocks(
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": [{"type": "text", "text": output}],
                    })],
                );
            }
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert(
        "messages".into(),
        Value::Array(
            messages
                .into_iter()
                .map(|(role, blocks)| json!({"role": role, "content": blocks}))
                .collect(),
        ),
    );
    if !system_parts.is_empty() {
        out.insert("system".into(), json!(system_parts.join("\n\n")));
    }

    let tools: Vec<Value> = request::parse_tools(request)
        .into_iter()
        .map(|tool| {
            let mut mapped = Map::new();
            mapped.insert("name".into(), json!(tool.name));
            if let Some(description) = tool.description {
                mapped.insert("description".into(), json!(description));
            }
            mapped.insert(
                "input_schema".into(),
                tool.parameters
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            Value::Object(mapped)
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }

    if let Some(choice) = request::parse_tool_choice(request) {
        let mapped = match choice {
            ToolChoice::Auto => json!({"type": "auto"}),
            ToolChoice::Required => json!({"type": "any"}),
            ToolChoice::None => json!({"type": "none"}),
            ToolChoice::Function(name) => json!({"type": "tool", "name": name}),
        };
        out.insert("tool_choice".into(), mapped);
    }

    let mut max_tokens = request
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(opts.default_max_tokens));

    if let Some(effort) = request::reasoning_effort(request) {
        let budget = thinking_budget(effort);
        if max_tokens <= budget {
            max_tokens = budget + 8_192;
        }
        out.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
        // Anthropic rejects sampling overrides alongside extended thinking, so
        // temperature/top_p are intentionally dropped here.
    } else {
        for key in ["temperature", "top_p"] {
            if let Some(value) = request.get(key).filter(|value| value.is_number()) {
                out.insert(key.into(), value.clone());
            }
        }
    }
    out.insert("max_tokens".into(), json!(max_tokens));

    if request::wants_stream(request) {
        out.insert("stream".into(), json!(true));
    }

    Ok(Value::Object(out))
}

fn content_block(part: &Part) -> Option<Value> {
    match part {
        Part::Text(text) => Some(json!({"type": "text", "text": text})),
        Part::Image(url) => image_block(url),
    }
}

fn image_block(url: &str) -> Option<Value> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (media_type, data) = rest.split_once(";base64,")?;
        Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }))
    } else if url.starts_with("http://") || url.starts_with("https://") {
        Some(json!({
            "type": "image",
            "source": {"type": "url", "url": url},
        }))
    } else {
        None
    }
}

pub fn anthropic_response_to_responses(
    upstream: &Value,
    requested_model: &str,
    response_id: &str,
    created_at: i64,
) -> Result<Value, TranslateError> {
    let content = upstream
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| TranslateError::new("upstream anthropic response has no content"))?;

    let mut output = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                output.push(response::reasoning_item(
                    &format!("rs_{}", output.len()),
                    block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ));
            }
            Some("text") => {
                output.push(response::message_item(
                    &format!("msg_{}", output.len()),
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "completed",
                ));
            }
            Some("tool_use") => {
                let arguments = block
                    .get("input")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "{}".to_string());
                output.push(response::function_call_item(
                    &format!("fc_{}", output.len()),
                    block.get("id").and_then(Value::as_str).unwrap_or_default(),
                    block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &arguments,
                    "completed",
                ));
            }
            // redacted_thinking and unknown block types are dropped.
            _ => {}
        }
    }

    let incomplete = upstream.get("stop_reason").and_then(Value::as_str) == Some("max_tokens");
    let usage = anthropic_usage(upstream.get("usage"));
    Ok(response::assemble(
        response_id,
        requested_model,
        created_at,
        output,
        usage,
        incomplete,
    ))
}

fn anthropic_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.filter(|usage| usage.is_object()) else {
        return Value::Null;
    };
    let input = usage.get("input_tokens").and_then(Value::as_i64);
    let output = usage.get("output_tokens").and_then(Value::as_i64);
    response::usage_value(
        input,
        output,
        None,
        usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        0,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn opts() -> TranslateOptions {
        TranslateOptions::default()
    }

    #[test]
    fn maps_instructions_and_inline_system_messages_into_system() {
        let out = responses_to_anthropic(
            &json!({
                "instructions": "Be terse.",
                "input": [
                    {"type": "message", "role": "developer", "content": "House rules."},
                    {"type": "message", "role": "user", "content": "hi"}
                ],
            }),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["system"], "Be terse.\n\nHouse rules.");
        let messages = out["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(out["max_tokens"], 32_768);
    }

    #[test]
    fn merges_consecutive_same_role_messages() {
        let out = responses_to_anthropic(
            &json!({"input": [
                {"type": "message", "role": "user", "content": "part one"},
                {"type": "function_call_output", "call_id": "c1", "output": "result"}
            ]}),
            "k3",
            &opts(),
        )
        .expect("translates");
        let messages = out["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        let blocks = messages[0]["content"].as_array().expect("blocks");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_result");
        assert_eq!(blocks[1]["tool_use_id"], "c1");
    }

    #[test]
    fn maps_function_calls_to_tool_use_with_parsed_input() {
        let out = responses_to_anthropic(
            &json!({"input": [
                {"type": "function_call", "call_id": "c1", "name": "ls", "arguments": "{\"path\":\".\"}"},
                {"type": "function_call", "call_id": "c2", "name": "bad", "arguments": "not json"}
            ]}),
            "k3",
            &opts(),
        )
        .expect("translates");
        let blocks = out["messages"][0]["content"].as_array().expect("blocks");
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["input"]["path"], ".");
        assert_eq!(blocks[1]["input"], json!({}));
    }

    #[test]
    fn maps_images_from_data_uri_and_url() {
        let out = responses_to_anthropic(
            &json!({"input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
                    {"type": "input_image", "image_url": {"url": "https://example.com/x.png"}}
                ]
            }]}),
            "k3",
            &opts(),
        )
        .expect("translates");
        let blocks = out["messages"][0]["content"].as_array().expect("blocks");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "AAAA");
        assert_eq!(blocks[1]["source"]["type"], "url");
    }

    #[test]
    fn maps_tools_and_tool_choice() {
        let out = responses_to_anthropic(
            &json!({
                "tools": [{"type": "function", "name": "get_weather", "description": "d", "parameters": {"type": "object"}}],
                "tool_choice": "required",
            }),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["tools"][0]["name"], "get_weather");
        assert_eq!(out["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(out["tool_choice"]["type"], "any");

        let out = responses_to_anthropic(
            &json!({
                "tools": [{"type": "function", "name": "bare"}],
                "tool_choice": {"type": "function", "name": "bare"},
            }),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(out["tool_choice"], json!({"type": "tool", "name": "bare"}));
    }

    #[test]
    fn maps_reasoning_effort_to_thinking_and_drops_sampling() {
        let out = responses_to_anthropic(
            &json!({"reasoning": {"effort": "high"}, "temperature": 0.3, "max_output_tokens": 40_000}),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["thinking"]["budget_tokens"], 16_384);
        assert_eq!(out["max_tokens"], 40_000);
        assert!(out.get("temperature").is_none());
    }

    #[test]
    fn bumps_max_tokens_above_thinking_budget() {
        let out = responses_to_anthropic(
            &json!({"reasoning": {"effort": "xhigh"}, "max_output_tokens": 1_000}),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["thinking"]["budget_tokens"], 24_576);
        assert_eq!(out["max_tokens"], 24_576 + 8_192);
    }

    #[test]
    fn keeps_sampling_params_without_thinking_and_sets_stream() {
        let out = responses_to_anthropic(
            &json!({"temperature": 0.3, "top_p": 0.8, "stream": true}),
            "k3",
            &opts(),
        )
        .expect("translates");
        assert_eq!(out["temperature"], 0.3);
        assert_eq!(out["top_p"], 0.8);
        assert_eq!(out["stream"], true);
    }

    #[test]
    fn translates_full_anthropic_response() {
        let response = anthropic_response_to_responses(
            &json!({
                "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "text", "text": "answer"},
                    {"type": "tool_use", "id": "toolu_1", "name": "ls", "input": {"path": "."}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 12, "output_tokens": 4, "cache_read_input_tokens": 6},
            }),
            "kimi/k3",
            "resp_1",
            9,
        )
        .expect("translates");
        let output = response["output"].as_array().expect("output");
        assert_eq!(output.len(), 3);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "hmm");
        assert_eq!(output[1]["content"][0]["text"], "answer");
        assert_eq!(output[2]["call_id"], "toolu_1");
        assert_eq!(output[2]["arguments"], "{\"path\":\".\"}");
        assert_eq!(response["usage"]["input_tokens"], 12);
        assert_eq!(response["usage"]["total_tokens"], 16);
        assert_eq!(
            response["usage"]["input_tokens_details"]["cached_tokens"],
            6
        );
        assert_eq!(response["status"], "completed");
    }

    #[test]
    fn maps_max_tokens_stop_to_incomplete() {
        let response = anthropic_response_to_responses(
            &json!({"content": [{"type": "text", "text": "cut"}], "stop_reason": "max_tokens"}),
            "m",
            "resp_2",
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
    fn errors_on_missing_content() {
        assert!(anthropic_response_to_responses(&json!({}), "m", "r", 1).is_err());
    }
}
