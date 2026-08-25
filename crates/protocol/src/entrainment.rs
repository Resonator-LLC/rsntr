//! Entrainment stream choreography.
//!
//! An entrain stream carries, in order, one acknowledgment (`rsntr:Done`,
//! or an early `rsntr:Denied` / `rsntr:Error`), then unbounded
//! `rsntr:Vibration` frames until the node damps the entrainment with an
//! `rsntr:Error`, a second `rsntr:Done` confirms an in-band `rsntr:Damp`,
//! or the stream closes. Unlike a request's
//! [`ResponseReader`](crate::response::ResponseReader), end-of-stream while
//! flowing is normal (entrainment is connection-scoped), not truncation.
//!
//! [`EntrainmentReader`] is the client-side ordered state machine over that
//! sequence: it consumes decoded frames and yields typed
//! [`EntrainmentEvent`]s, erroring (never panicking) on anything out of
//! order: a Vibration before the acknowledgment, a mismatched request id or
//! point, a gap or duplicate in `rsntr:seq`, or a frame after termination.

use crate::envelope::{Denied, EnvelopeObject, ErrorEnvelope, Vibration};
use crate::error::ProtocolError;
use crate::request::envelope_class_name;

/// One typed event yielded by [`EntrainmentReader::accept`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntrainmentEvent {
    /// The `rsntr:Done` acknowledgment: the caller is entrained.
    Entrained,
    /// One vibration from the entrained point.
    Vibration(Vibration),
    /// A second `rsntr:Done` while flowing: the node confirming a
    /// `rsntr:Damp`; the stream is complete.
    Damped,
    /// An early `rsntr:Denied`; the stream is complete.
    Denied(Denied),
    /// An `rsntr:Error` (early `point-unknown`, or a later damp such as
    /// `limit-exceeded`); the stream is complete.
    Error(ErrorEnvelope),
}

#[derive(Debug)]
enum State {
    /// Nothing consumed yet; expecting Done, Denied, or Error.
    AwaitAck,
    /// Entrained; expecting Vibrations or a damping Error.
    Flowing,
    /// A terminal frame was consumed; nothing more may arrive.
    Finished,
    /// A choreography error occurred; the stream is poisoned.
    Failed,
}

/// Ordered state machine over one entrainment's frames.
#[derive(Debug)]
pub struct EntrainmentReader {
    id: String,
    point: String,
    state: State,
    next_seq: i64,
}

impl EntrainmentReader {
    /// Creates a reader for the entrainment identified by the
    /// `rsntr:Entrain` request's ULID string and the entrained point's IRI.
    pub fn new(id: impl Into<String>, point: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            point: point.into(),
            state: State::AwaitAck,
            next_seq: 0,
        }
    }

    /// True while the entrainment is acknowledged and vibrations may arrive.
    pub fn is_flowing(&self) -> bool {
        matches!(self.state, State::Flowing)
    }

    /// True once a terminal frame (Damped, Denied, Error) was consumed.
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }

    /// Consumes one decoded frame, yielding its typed event. After the first
    /// error the reader is poisoned and every further frame errors too.
    pub fn accept(&mut self, obj: EnvelopeObject) -> Result<EntrainmentEvent, ProtocolError> {
        match self.try_accept(obj) {
            Ok(ev) => Ok(ev),
            Err(e) => {
                self.state = State::Failed;
                Err(e)
            }
        }
    }

    /// Call at end of stream. End-of-stream while flowing is a normal end
    /// of entrainment (connection-scoped); before the acknowledgment it is
    /// truncation.
    pub fn finish(&self) -> Result<(), ProtocolError> {
        match self.state {
            State::Flowing | State::Finished => Ok(()),
            State::AwaitAck => Err(ProtocolError::choreography(
                "entrain stream ended before the acknowledgment (truncated)",
            )),
            State::Failed => Err(ProtocolError::choreography("entrain stream already failed")),
        }
    }

    fn try_accept(&mut self, obj: EnvelopeObject) -> Result<EntrainmentEvent, ProtocolError> {
        match self.state {
            State::Failed => {
                return Err(ProtocolError::choreography("entrain stream already failed"));
            }
            State::Finished => {
                return Err(ProtocolError::choreography(format!(
                    "{} frame after the entrainment completed",
                    envelope_class_name(&obj)
                )));
            }
            State::AwaitAck | State::Flowing => {}
        }
        let flowing = matches!(self.state, State::Flowing);

        match obj {
            EnvelopeObject::Done(d) => {
                self.check_id(Some(&d.id), "rsntr:Done")?;
                if flowing {
                    // The node confirming a Damp: terminal.
                    self.state = State::Finished;
                    Ok(EntrainmentEvent::Damped)
                } else {
                    self.state = State::Flowing;
                    Ok(EntrainmentEvent::Entrained)
                }
            }
            EnvelopeObject::Vibration(v) => {
                if !flowing {
                    return Err(ProtocolError::choreography(
                        "rsntr:Vibration frame before the rsntr:Done acknowledgment",
                    ));
                }
                self.check_id(Some(&v.id), "rsntr:Vibration")?;
                if v.point != self.point {
                    return Err(ProtocolError::choreography(format!(
                        "vibration names point <{}>, expected <{}>",
                        v.point, self.point
                    )));
                }
                if v.seq != self.next_seq {
                    return Err(ProtocolError::choreography(format!(
                        "vibration seq {} out of order (expected {})",
                        v.seq, self.next_seq
                    )));
                }
                self.next_seq += 1;
                Ok(EntrainmentEvent::Vibration(v))
            }
            EnvelopeObject::Denied(d) => {
                if flowing {
                    return Err(ProtocolError::choreography(
                        "rsntr:Denied frame after the acknowledgment",
                    ));
                }
                self.check_id(d.id.as_deref(), "rsntr:Denied")?;
                self.state = State::Finished;
                Ok(EntrainmentEvent::Denied(d))
            }
            EnvelopeObject::Error(e) => {
                // Valid both before the ack (point-unknown) and while
                // flowing (the node damping, e.g. limit-exceeded).
                self.check_id(e.id.as_deref(), "rsntr:Error")?;
                self.state = State::Finished;
                Ok(EntrainmentEvent::Error(e))
            }
            other => Err(ProtocolError::choreography(format!(
                "{} frame is not an entrainment object",
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
