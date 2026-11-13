//! A parser for server-sent event streams as model providers write them.
//!
//! It follows the event stream format (lines ending in LF, CR or CRLF;
//! `field: value`; a blank line ends an event; lines starting with a
//! colon are comments) and is lenient where providers are sloppy: a
//! `data:` line whose value is complete JSON, or `[DONE]`, is an event at
//! once, without waiting for the blank line that some servers forget, and
//! a last event that is not followed by a blank line is still delivered
//! when the stream ends. Bytes can arrive split anywhere, including
//! inside a multi-byte character or between CR and LF.

/// One event of a stream: the `data` lines joined with newlines, and the
/// `event` name if there was one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// The longest line and the largest event accepted, in bytes; beyond
/// that the stream is broken or hostile.
pub const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// The stream is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the event stream has an event larger than {MAX_EVENT_BYTES} bytes")]
pub struct SseOverflow;

/// An incremental parser: feed it bytes as they come.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the line being received.
    line: Vec<u8>,
    /// Whether the last byte seen was a CR, so that a following LF
    /// belongs to the same line end.
    after_cr: bool,
    event: Option<String>,
    data: Vec<String>,
    /// Bytes held in `data`.
    data_bytes: usize,
    at_start: bool,
}

/// Whether a `data` value is complete on its own: providers send one JSON
/// document, or `[DONE]`, per line.
fn is_complete(data: &str) -> bool {
    let data = data.trim();
    data == "[DONE]"
        || (data.starts_with('{') && serde_json::from_str::<serde::de::IgnoredAny>(data).is_ok())
}

impl SseParser {
    pub fn new() -> Self {
        SseParser {
            at_start: true,
            ..SseParser::default()
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            self.event = None;
            return;
        }
        out.push(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
        });
        self.data_bytes = 0;
    }

    fn line_done(&mut self, out: &mut Vec<SseEvent>) -> Result<(), SseOverflow> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = String::from_utf8_lossy(&bytes).into_owned();
        if std::mem::take(&mut self.at_start)
            && let Some(rest) = line.strip_prefix('\u{feff}')
        {
            line = rest.to_owned();
        }
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "data" => {
                // A complete value following another one: the server left
                // out the blank line between two events.
                if !self.data.is_empty() && is_complete(value) && is_complete(&self.data.join("\n"))
                {
                    self.dispatch(out);
                }
                self.data_bytes += value.len();
                if self.data_bytes > MAX_EVENT_BYTES {
                    return Err(SseOverflow);
                }
                self.data.push(value.to_owned());
                if value.trim() == "[DONE]" {
                    self.dispatch(out);
                }
            }
            "event" => self.event = Some(value.to_owned()),
            // `id` and `retry` mean nothing for a one-shot response;
            // unknown fields are ignored, as the format says.
            _ => {}
        }
        Ok(())
    }

    /// Feeds bytes; returns the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseOverflow> {
        let mut out = Vec::new();
        for &byte in bytes {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' => self.line_done(&mut out)?,
                b'\r' => {
                    self.after_cr = true;
                    self.line_done(&mut out)?;
                }
                _ => {
                    self.line.push(byte);
                    if self.line.len() > MAX_EVENT_BYTES {
                        return Err(SseOverflow);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The stream has ended: returns what was still pending.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            // An overflow cannot happen here: the line is within bounds.
            let _ = self.line_done(&mut out);
        }
        self.dispatch(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk).unwrap());
        }
        events.extend(parser.finish());
        events
    }

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|event| event.data.as_str()).collect()
    }

    /// A table of streams and the data of the events they hold.
    #[test]
    #[allow(clippy::type_complexity)]
    fn nasty_streams() {
        let cases: &[(&str, &[&[u8]], &[&str])] = &[
            (
                "plain",
                &[b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\n"],
                &["{\"a\":1}", "{\"a\":2}"],
            ),
            (
                "crlf",
                &[b"data: {\"a\":1}\r\n\r\ndata: [DONE]\r\n\r\n"],
                &["{\"a\":1}", "[DONE]"],
            ),
            (
                "cr only",
                &[b"data: {\"a\":1}\r\rdata: {\"a\":2}\r\r"],
                &["{\"a\":1}", "{\"a\":2}"],
            ),
            (
                "crlf split between chunks",
                &[b"data: {\"a\":1}\r", b"\n\r", b"\ndata: {\"a\":2}\r\n\r\n"],
                &["{\"a\":1}", "{\"a\":2}"],
            ),
            (
                "comments and keep-alives",
                &[b": OPENROUTER PROCESSING\n\n:\n\ndata: {\"a\":1}\n\n: ping\n\n"],
                &["{\"a\":1}"],
            ),
            (
                "no space after the colon",
                &[b"data:{\"a\":1}\n\n"],
                &["{\"a\":1}"],
            ),
            (
                "only one leading space is dropped",
                &[b"data:  two spaces\n\n"],
                &[" two spaces"],
            ),
            (
                "split in the middle of a line",
                &[b"da", b"ta: {\"a\"", b":1}\n", b"\n"],
                &["{\"a\":1}"],
            ),
            (
                "split inside a multi-byte character",
                &[b"data: {\"t\":\"\xc3", b"\xa9\xe2\x82", b"\xac\"}\n\n"],
                &["{\"t\":\"é€\"}"],
            ),
            (
                "byte by byte",
                &[b"d", b"a", b"t", b"a", b":", b" ", b"x", b"\n", b"\n"],
                &["x"],
            ),
            (
                "missing blank lines between events",
                &[b"data: {\"a\":1}\ndata: {\"a\":2}\ndata: [DONE]\n"],
                &["{\"a\":1}", "{\"a\":2}", "[DONE]"],
            ),
            (
                "multi-line data is joined",
                &[b"data: {\"a\":\ndata: 1}\n\n"],
                &["{\"a\":\n1}"],
            ),
            (
                "no blank line at the end of the stream",
                &[b"data: {\"a\":1}\n\ndata: {\"a\":2}"],
                &["{\"a\":1}", "{\"a\":2}"],
            ),
            ("no newline at all", &[b"data: {\"a\":1}"], &["{\"a\":1}"]),
            (
                "event, id and retry fields",
                &[b"event: message\nid: 7\nretry: 100\ndata: {\"a\":1}\n\n"],
                &["{\"a\":1}"],
            ),
            (
                "unknown fields and bare field names",
                &[b"foo: bar\nbaz\ndata\n\n"],
                &[""],
            ),
            ("a byte order mark", &[b"\xef\xbb\xbfdata: x\n\n"], &["x"]),
            (
                "events without data are dropped",
                &[b"event: ping\n\n\n\n"],
                &[],
            ),
            ("empty stream", &[b""], &[]),
            ("done without json", &[b"data: [DONE]\n\n"], &["[DONE]"]),
            (
                "done right after an event without a blank line",
                &[b"data: {\"a\":1}\ndata: [DONE]\n\n"],
                &["{\"a\":1}", "[DONE]"],
            ),
            (
                "invalid utf-8 does not stop the stream",
                &[b"data: \xff\xfe\n\ndata: ok\n\n"],
                &["\u{fffd}\u{fffd}", "ok"],
            ),
        ];
        for (name, chunks, expected) in cases {
            let events = parse_all(chunks);
            assert_eq!(&data(&events), expected, "{name}");
        }
    }

    #[test]
    fn event_names_are_kept() {
        let events = parse_all(&[b"event: error\ndata: {\"message\":\"x\"}\n\ndata: y\n\n"]);
        assert_eq!(events[0].event.as_deref(), Some("error"));
        assert_eq!(events[1].event, None);
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let stream = "data: {\"t\":\"h\u{e9}llo \u{1f30a}\"}\r\n\r\n: comment\r\n\
                      data: {\"t\":\"two\"}\n\ndata: [DONE]\n\n";
        let whole = parse_all(&[stream.as_bytes()]);
        assert_eq!(whole.len(), 3);
        for split in 0..=stream.len() {
            let (a, b) = stream.as_bytes().split_at(split);
            assert_eq!(parse_all(&[a, b]), whole, "split at {split}");
        }
    }

    #[test]
    fn oversized_events_are_refused() {
        let mut parser = SseParser::new();
        let chunk = vec![b'a'; 1024 * 1024];
        let mut result = Ok(Vec::new());
        for _ in 0..5 {
            result = parser.feed(&chunk);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(SseOverflow));
    }
}
