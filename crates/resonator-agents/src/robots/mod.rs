//! External robots: address policy, request signing and capabilities.
//! TODO(handoff): connector (webhook `EventSink`, capabilities fetcher)
//! and the `/robot/events` stream hub are not written yet.

pub mod capabilities;
pub mod connector;
pub mod sign;
pub mod ssrf;
