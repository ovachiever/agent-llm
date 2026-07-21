//! Shared parsing of incoming Responses API requests into neutral shapes the
//! dialect builders consume.

use serde_json::Value;

use crate::TranslateError;

pub(crate) enum Part {
    Text(String),
    Image(String),
}

pub(crate) enum InputItem {
    Message {
        role: String,
        parts: Vec<Part>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

pub(crate) struct FunctionTool {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
}

pub(crate) enum ToolChoice {
    Auto,
    Required,
    None,
    Function(String),
}

pub(crate) fn ensure_supported(request: &Value) -> Result<(), TranslateError> {
    if request
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
    {
        return Err(TranslateError::new(
            "previous_response_id is not supported; resend full conversation input",
        ));
    }
    Ok(())
}

pub(crate) fn parse_input(request: &Value) -> Result<Vec<InputItem>, TranslateError> {
    match request.get("input") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![InputItem::Message {
            role: "user".into(),
            parts: vec![Part::Text(text.clone())],
        }]),
        Some(Value::Array(items)) => Ok(items.iter().filter_map(parse_item).collect()),
        Some(_) => Err(TranslateError::new("`input` must be a string or an array")),
    }
}

fn parse_item(item: &Value) -> Option<InputItem> {
    // Items without an explicit type but with a role are messages.
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match item_type {
        "message" => Some(InputItem::Message {
            role: item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string(),
            parts: parse_content(item.get("content")),
        }),
        "function_call" => Some(InputItem::FunctionCall {
            call_id: str_field(item, "call_id"),
            name: str_field(item, "name"),
            arguments: str_field(item, "arguments"),
        }),
        "function_call_output" => Some(InputItem::FunctionCallOutput {
            call_id: str_field(item, "call_id"),
            output: stringify(item.get("output")),
        }),
        // Reasoning items (and anything unrecognized) cannot be replayed to a
        // different upstream; drop them.
        _ => None,
    }
}

fn parse_content(content: Option<&Value>) -> Vec<Part> {
    match content {
        Some(Value::String(text)) => vec![Part::Text(text.clone())],
        Some(Value::Array(parts)) => parts.iter().filter_map(parse_part).collect(),
        _ => Vec::new(),
    }
}

fn parse_part(part: &Value) -> Option<Part> {
    match part.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text" | "text") => Some(Part::Text(str_field(part, "text"))),
        Some("input_image") => part.get("image_url").and_then(image_url).map(Part::Image),
        _ => None,
    }
}

fn image_url(value: &Value) -> Option<String> {
    match value {
        Value::String(url) => Some(url.clone()),
        Value::Object(map) => map
            .get("url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

pub(crate) fn parse_tools(request: &Value) -> Vec<FunctionTool> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| {
                    tool.get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("function")
                        == "function"
                })
                .filter_map(|tool| {
                    Some(FunctionTool {
                        name: tool.get("name").and_then(Value::as_str)?.to_string(),
                        description: tool
                            .get("description")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        parameters: tool.get("parameters").filter(|p| !p.is_null()).cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn parse_tool_choice(request: &Value) -> Option<ToolChoice> {
    match request.get("tool_choice")? {
        Value::String(choice) => match choice.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "required" => Some(ToolChoice::Required),
            "none" => Some(ToolChoice::None),
            _ => None,
        },
        Value::Object(map) => map
            .get("name")
            .and_then(Value::as_str)
            .map(|name| ToolChoice::Function(name.to_string())),
        _ => None,
    }
}

pub(crate) fn reasoning_effort(request: &Value) -> Option<&str> {
    request
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .filter(|effort| !effort.is_empty())
}

pub(crate) fn wants_stream(request: &Value) -> bool {
    request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn stringify(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn string_input_becomes_a_user_message() {
        let items = parse_input(&json!({"input": "hello"})).expect("parses");
        assert_eq!(items.len(), 1);
        let InputItem::Message { role, parts } = &items[0] else {
            panic!("expected message");
        };
        assert_eq!(role, "user");
        assert!(matches!(&parts[0], Part::Text(text) if text == "hello"));
    }

    #[test]
    fn reasoning_and_unknown_items_are_dropped() {
        let items = parse_input(&json!({"input": [
            {"type": "reasoning", "summary": []},
            {"type": "computer_screenshot"},
            {"type": "message", "role": "user", "content": "hi"}
        ]}))
        .expect("parses");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn function_call_output_stringifies_structured_output() {
        let items = parse_input(&json!({"input": [
            {"type": "function_call_output", "call_id": "c1", "output": {"ok": true}}
        ]}))
        .expect("parses");
        let InputItem::FunctionCallOutput { output, .. } = &items[0] else {
            panic!("expected function_call_output");
        };
        assert_eq!(output, "{\"ok\":true}");
    }

    #[test]
    fn rejects_previous_response_id() {
        let error =
            ensure_supported(&json!({"previous_response_id": "resp_1"})).expect_err("must reject");
        assert!(error.message.contains("previous_response_id"));
        assert!(ensure_supported(&json!({"previous_response_id": null})).is_ok());
    }

    #[test]
    fn non_function_tools_are_skipped() {
        let tools = parse_tools(&json!({"tools": [
            {"type": "web_search"},
            {"type": "function", "name": "get_weather", "parameters": {"type": "object"}}
        ]}));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
    }
}
