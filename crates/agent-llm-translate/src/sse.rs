//! Incremental server-sent-events wire parser.
//!
//! Bytes go in (in arbitrary chunk boundaries, including mid-line and
//! mid-UTF-8-codepoint splits); complete events come out. Partial frames are
//! buffered across `push` calls.

#[derive(Debug, Clone)]
pub struct SseEvent {
    /// The `event:` field, when the upstream names its events (Anthropic does;
    /// Chat Completions streams are data-only).
    pub event: Option<String>,
    /// All `data:` lines of the frame, joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\n', '\r']);

            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(SseEvent {
                        event: self.event.take(),
                        data: self.data.join("\n"),
                    });
                }
                self.event = None;
                self.data.clear();
                continue;
            }

            if let Some(rest) = line.strip_prefix("data:") {
                self.data
                    .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("event:") {
                self.event = Some(rest.trim().to_string());
            }
            // `id:`, `retry:`, and comment (`:`) lines are ignored.
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_data_frame() {
        let mut parser = SseParser::new();
        let events = parser.push(b"data: {\"a\":1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[0].event, None);
    }

    #[test]
    fn buffers_frames_split_across_pushes() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: {\"a\":").is_empty());
        assert!(parser.push(b"1}").is_empty());
        let events = parser.push(b"\n\ndata: [DONE]\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].data, "[DONE]");
    }

    #[test]
    fn joins_multiple_data_lines_with_newline() {
        let mut parser = SseParser::new();
        let events = parser.push(b"data: first\ndata: second\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "first\nsecond");
    }

    #[test]
    fn captures_the_event_field_and_resets_it_per_frame() {
        let mut parser = SseParser::new();
        let events = parser.push(b"event: message_start\ndata: {}\n\ndata: {\"n\":2}\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[1].event, None);
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let mut parser = SseParser::new();
        let events = parser.push(b"event: ping\r\ndata: {}\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("ping"));
        assert_eq!(events[0].data, "{}");
    }

    #[test]
    fn ignores_comments_ids_and_dataless_frames() {
        let mut parser = SseParser::new();
        let events = parser.push(b": keepalive\n\nid: 7\nretry: 100\n\ndata: real\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real");
    }

    #[test]
    fn survives_utf8_split_across_pushes() {
        let mut parser = SseParser::new();
        let frame = "data: héllo\n\n".as_bytes();
        assert!(parser.push(&frame[..7]).is_empty());
        let events = parser.push(&frame[7..]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "héllo");
    }
}
