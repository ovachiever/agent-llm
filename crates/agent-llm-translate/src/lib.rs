//! Protocol translation between the OpenAI Responses API and upstream
//! provider dialects (OpenAI Chat Completions, Anthropic Messages).
//!
//! Everything here is sans-IO: pure transformations over `serde_json::Value`
//! plus incremental state machines for SSE streams. The gateway owns all
//! networking and drives these types.

mod anthropic;
mod chat;
mod request;
mod response;
mod reverse;
mod sse;
mod stream;

use std::fmt;

use serde_json::Value;

pub use anthropic::{anthropic_response_to_responses, responses_to_anthropic};
pub use chat::{chat_response_to_responses, responses_to_chat};
pub use reverse::{ReverseStreamTranslator, anthropic_to_chat, chat_response_to_anthropic};
pub use sse::{SseEvent, SseParser};
pub use stream::{OutEvent, StreamTranslator};

/// The wire protocol an upstream provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    ChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone)]
pub struct TranslateOptions {
    /// Anthropic requires `max_tokens`; this is used when the incoming
    /// request does not carry `max_output_tokens`.
    pub default_max_tokens: u32,
}

impl Default for TranslateOptions {
    fn default() -> Self {
        Self {
            default_max_tokens: 32_768,
        }
    }
}

#[derive(Debug)]
pub struct TranslateError {
    pub message: String,
}

impl TranslateError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TranslateError {}

/// Extract `(input_tokens, output_tokens, total_tokens)` from a translated
/// Responses object's `usage`.
pub fn usage_from_responses(response: &Value) -> (Option<i64>, Option<i64>, Option<i64>) {
    let usage = response.get("usage");
    let read = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_i64)
    };
    (
        read("input_tokens"),
        read("output_tokens"),
        read("total_tokens"),
    )
}

/// Format one translated event as an SSE frame.
pub fn format_sse(event: &OutEvent) -> String {
    format!("event: {}\ndata: {}\n\n", event.event, event.data)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn default_options_carry_anthropic_max_tokens() {
        assert_eq!(TranslateOptions::default().default_max_tokens, 32_768);
    }

    #[test]
    fn usage_from_responses_reads_translated_usage() {
        let response =
            json!({"usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}});
        assert_eq!(
            usage_from_responses(&response),
            (Some(10), Some(5), Some(15))
        );
    }

    #[test]
    fn usage_from_responses_handles_null_usage() {
        assert_eq!(
            usage_from_responses(&json!({"usage": null})),
            (None, None, None)
        );
        assert_eq!(usage_from_responses(&json!({})), (None, None, None));
    }

    #[test]
    fn format_sse_emits_event_and_compact_data() {
        let event = OutEvent {
            event: "response.completed".into(),
            data: json!({"type": "response.completed", "sequence_number": 3}),
        };
        let frame = format_sse(&event);
        assert!(frame.starts_with("event: response.completed\ndata: "));
        assert!(frame.ends_with("\n\n"));
        assert!(frame.contains("\"sequence_number\":3"));
    }
}
