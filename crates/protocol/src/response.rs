//! Response stream choreography.
//!
//! The receive half of a request stream carries, in order, either:
//!
//! - a header frame (`rsntr:Result`), zero or more row-batch frames, and a
//!   trailer (`rsntr:Done`, or `rsntr:Error` if execution failed
//!   mid-stream); or
//! - one or more `rsntr:Graph` frames (sparql CONSTRUCT results) followed
//!   by a `rsntr:Done` trailer (v3); or
//! - one or more passthrough [`Generic`] frames followed by `rsntr:Done`
//!   (v3, mod responses in classes this codec does not know); or
//! - a single terminal frame: `rsntr:Denied`, `rsntr:Error`, `rsntr:Help`,
//!   or `rsntr:Media` (after which the stream is raw bytes, not frames).
//!
//! [`ResponseReader`] is the ordered state machine over that sequence: it
//! consumes decoded frames ([`EnvelopeObject`]s) and yields typed
//! [`ResponseEvent`]s, erroring (never panicking) on anything out of order.

use crate::envelope::{
    Denied, Done, EnvelopeObject, ErrorEnvelope, Generic, Graph, Help, Media, ResultHeader, Row,
};
use crate::error::ProtocolError;
use crate::request::envelope_class_name;

/// One typed event yielded by [`ResponseReader::accept`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseEvent {
    /// The `rsntr:Result` header: columns and declared types, in order.
    Header(ResultHeader),
    /// One row batch, seq-checked and column-checked against the header.
    Rows(Vec<Row>),
    /// One graph chunk (v3), id- and seq-checked.
    Graph(Graph),
    /// One passthrough frame in an unknown `rsntr:` class (v3).
    Generic(Generic),
    /// The `rsntr:Done` trailer; the stream is complete.
    Done(Done),
    /// An early `rsntr:Denied`; the stream is complete.
    Denied(Denied),
    /// An `rsntr:Error`, before the header or mid-stream; the stream is
    /// complete.
    Error(ErrorEnvelope),
    /// A `rsntr:Help` response: the whole response is this one frame. The
    /// stream is complete.
    Help {
        /// The usage guidance prose.
        signal: String,
        /// Names of drill-down help topics, in document order.
        topics: Vec<String>,
    },
    /// A `rsntr:Media` go-ahead header: everything after this frame on the
    /// stream is the raw media byte feed. The framed stream is complete.
    Media(Media),
}

impl ResponseEvent {
    /// True for the events that end the framed stream.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ResponseEvent::Done(_)
                | ResponseEvent::Denied(_)
                | ResponseEvent::Error(_)
                | ResponseEvent::Help { .. }
                | ResponseEvent::Media(_)
        )
    }
}

#[derive(Debug)]
enum State {
    /// Nothing consumed yet.
    AwaitHeader,
    /// Result header seen; expecting Row batches, Done, or Error.
    Rows { columns: Vec<String> },
    /// A Graph frame seen; expecting more Graphs, Done, or Error.
    Graphs,
    /// A Generic frame seen; expecting more Generics, Done, or Error.
    Generics,
    /// A terminal frame was consumed; nothing more may arrive.
    Finished,
    /// A choreography error occurred; the stream is poisoned.
    Failed,
}

/// Ordered state machine over one request's response frames.
///
/// Construct with the request's wire id (the ULID string), feed each decoded
/// frame to [`accept`](Self::accept), and call [`finish`](Self::finish) when
/// the transport reports end of stream to catch truncation (a stream that
/// ended without a terminal frame).
#[derive(Debug)]
pub struct ResponseReader {
    id: String,
    state: State,
    next_seq: i64,
}

impl ResponseReader {
    /// Creates a reader correlating frames against `id` (wire ULID string).
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            state: State::AwaitHeader,
            next_seq: 0,
        }
    }

    /// True once a terminal frame was consumed.
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }

    /// Consumes one decoded frame, yielding its typed event.
    ///
    /// Any out-of-order, duplicated, mismatched, or non-response frame is a
    /// [`ProtocolError::Choreography`]; after the first error the reader is
    /// poisoned and every further frame errors too. Never panics.
    pub fn accept(&mut self, obj: EnvelopeObject) -> Result<ResponseEvent, ProtocolError> {
        match self.try_accept(obj) {
            Ok(ev) => Ok(ev),
            Err(e) => {
                self.state = State::Failed;
                Err(e)
            }
        }
    }

    /// Call at end of stream: errors if the stream was truncated (no
    /// terminal frame arrived) or previously failed.
    pub fn finish(&self) -> Result<(), ProtocolError> {
        match self.state {
            State::Finished => Ok(()),
            State::AwaitHeader => Err(ProtocolError::choreography(
                "response stream ended before any frame (truncated: no header)",
            )),
            State::Rows { .. } | State::Graphs | State::Generics => Err(
                ProtocolError::choreography("response stream ended without a Done trailer"),
            ),
            State::Failed => Err(ProtocolError::choreography(
                "response stream already failed",
            )),
        }
    }

    fn try_accept(&mut self, obj: EnvelopeObject) -> Result<ResponseEvent, ProtocolError> {
        match &self.state {
            State::Failed => {
                return Err(ProtocolError::choreography(
                    "response stream already failed",
                ));
            }
            State::Finished => {
                return Err(ProtocolError::choreography(format!(
                    "{} frame after the response completed",
                    envelope_class_name(&obj)
                )));
            }
            _ => {}
        }
        let at_start = matches!(self.state, State::AwaitHeader);

        match obj {
            EnvelopeObject::Result(h) => {
                if !at_start {
                    return Err(ProtocolError::choreography(
                        "rsntr:Result header out of order",
                    ));
                }
                self.check_id(Some(&h.id), "rsntr:Result")?;
                self.state = State::Rows {
                    columns: h.columns.clone(),
                };
                Ok(ResponseEvent::Header(h))
            }
            EnvelopeObject::Row(rows) => {
                let State::Rows { columns } = &self.state else {
                    return Err(ProtocolError::choreography(
                        "rsntr:Row frame before the rsntr:Result header",
                    ));
                };
                if rows.is_empty() {
                    return Err(ProtocolError::choreography("empty row batch frame"));
                }
                for row in &rows {
                    if row.seq != self.next_seq {
                        return Err(ProtocolError::choreography(format!(
                            "row seq {} out of order (expected {})",
                            row.seq, self.next_seq
                        )));
                    }
                    for (name, _) in &row.cells {
                        if !columns.iter().any(|c| c == name) {
                            return Err(ProtocolError::choreography(format!(
                                "row cell names column {name:?} not declared by the header"
                            )));
                        }
                    }
                    self.next_seq += 1;
                }
                Ok(ResponseEvent::Rows(rows))
            }
            EnvelopeObject::Graph(gr) => {
                if !at_start && !matches!(self.state, State::Graphs) {
                    return Err(ProtocolError::choreography(
                        "rsntr:Graph frame out of order",
                    ));
                }
                self.check_id(Some(&gr.id), "rsntr:Graph")?;
                if gr.seq != self.next_seq {
                    return Err(ProtocolError::choreography(format!(
                        "graph seq {} out of order (expected {})",
                        gr.seq, self.next_seq
                    )));
                }
                self.next_seq += 1;
                self.state = State::Graphs;
                Ok(ResponseEvent::Graph(gr))
            }
            EnvelopeObject::Generic(gnrc) => {
                if !at_start && !matches!(self.state, State::Generics) {
                    return Err(ProtocolError::choreography(
                        "generic passthrough frame out of order",
                    ));
                }
                self.state = State::Generics;
                Ok(ResponseEvent::Generic(gnrc))
            }
            EnvelopeObject::Done(d) => {
                if at_start {
                    return Err(ProtocolError::choreography(
                        "rsntr:Done frame before any response frame",
                    ));
                }
                self.check_id(Some(&d.id), "rsntr:Done")?;
                self.state = State::Finished;
                Ok(ResponseEvent::Done(d))
            }
            EnvelopeObject::Denied(d) => {
                if !at_start {
                    // The authenticator runs before execution; a denial can
                    // only be the first response frame.
                    return Err(ProtocolError::choreography(
                        "rsntr:Denied frame after the response started",
                    ));
                }
                self.check_id(d.id.as_deref(), "rsntr:Denied")?;
                self.state = State::Finished;
                Ok(ResponseEvent::Denied(d))
            }
            EnvelopeObject::Error(e) => {
                // Errors are valid both before the header and mid-stream.
                self.check_id(e.id.as_deref(), "rsntr:Error")?;
                self.state = State::Finished;
                Ok(ResponseEvent::Error(e))
            }
            EnvelopeObject::Help(h) => {
                // A help response is the whole response: exactly one frame,
                // in place of a Result header.
                if !at_start {
                    return Err(ProtocolError::choreography(
                        "rsntr:Help frame after the response started",
                    ));
                }
                let Help { id, signal, topics } = h;
                self.check_id(id.as_deref(), "rsntr:Help")?;
                self.state = State::Finished;
                Ok(ResponseEvent::Help { signal, topics })
            }
            EnvelopeObject::Media(m) => {
                // The media go-ahead replaces the Result header; the raw
                // byte feed after it is not frames, so the framed stream is
                // complete from the reader's point of view.
                if !at_start {
                    return Err(ProtocolError::choreography(
                        "rsntr:Media frame after the response started",
                    ));
                }
                self.check_id(Some(&m.id), "rsntr:Media")?;
                self.state = State::Finished;
                Ok(ResponseEvent::Media(m))
            }
            other => Err(ProtocolError::choreography(format!(
                "{} frame is not a response object",
                envelope_class_name(&other)
            ))),
        }
    }

    /// Checks a frame's request id against the reader's. `None` (Denied and
    /// Error frames may omit it) correlates by stream and is accepted.
    fn check_id(&self, id: Option<&str>, class: &str) -> Result<(), ProtocolError> {
        match id {
            None => Ok(()),
            Some(id) if id == self.id => Ok(()),
            Some(id) => Err(ProtocolError::choreography(format!(
                "{class} frame carries request id {id:?}, expected {:?}",
                self.id
            ))),
        }
    }
}
