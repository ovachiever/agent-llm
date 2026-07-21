//! Builders for Responses API objects shared by the non-streaming
//! translators and the stream state machine.

use serde_json::{Value, json};

pub(crate) fn skeleton(id: &str, model: &str, created_at: i64, status: &str) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "model": model,
        "output": [],
        "usage": Value::Null,
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "parallel_tool_calls": true,
        "tools": [],
        "tool_choice": "auto",
    })
}

pub(crate) fn assemble(
    id: &str,
    model: &str,
    created_at: i64,
    output: Vec<Value>,
    usage: Value,
    incomplete: bool,
) -> Value {
    let status = if incomplete {
        "incomplete"
    } else {
        "completed"
    };
    let mut response = skeleton(id, model, created_at, status);
    response["output"] = Value::Array(output);
    response["usage"] = usage;
    if incomplete {
        response["incomplete_details"] = json!({"reason": "max_output_tokens"});
    }
    response
}

/// Build a Responses `usage` object; `Value::Null` when nothing was reported.
pub(crate) fn usage_value(
    input: Option<i64>,
    output: Option<i64>,
    total: Option<i64>,
    cached: i64,
    reasoning: i64,
) -> Value {
    if input.is_none() && output.is_none() && total.is_none() {
        return Value::Null;
    }
    let input = input.unwrap_or(0);
    let output = output.unwrap_or(0);
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": total.unwrap_or(input + output),
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens_details": {"reasoning_tokens": reasoning},
    })
}

pub(crate) fn reasoning_item(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": text}],
    })
}

pub(crate) fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": status,
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

pub(crate) fn function_call_item(
    id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    status: &str,
) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_marks_incomplete_responses() {
        let response = assemble("resp_1", "kimi/k3", 1, vec![], Value::Null, true);
        assert_eq!(response["status"], "incomplete");
        assert_eq!(
            response["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn usage_value_is_null_when_nothing_reported() {
        assert!(usage_value(None, None, None, 0, 0).is_null());
    }

    #[test]
    fn usage_value_computes_missing_total() {
        let usage = usage_value(Some(10), Some(5), None, 2, 0);
        assert_eq!(usage["total_tokens"], 15);
        assert_eq!(usage["input_tokens_details"]["cached_tokens"], 2);
    }
}
