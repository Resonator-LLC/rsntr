//! The mod handler seam: how the pipeline delegates requests for
//! modulations it does not implement itself.
//!
//! `resonator-node` must not depend on the extism runtime, and
//! [`RequestStream`](resonator_transport::RequestStream) is not object
//! safe, so the seam is a dyn trait that never sees the stream: the
//! handler pushes response frames into an mpsc sender and the pipeline
//! forwards them to the stream (the same forwarder pattern SQL execution
//! uses). Registered via [`Node::set_mod_handler`](crate::Node::set_mod_handler);
//! requests whose modulation matches none of [`ModHandler::mods`] still
//! answer `mod-unsupported`.

use std::future::Future;
use std::pin::Pin;

use resonator_protocol::{EnvelopeObject, Request};
use tokio::sync::mpsc;

/// The boxed future a [`ModHandler`] returns; borrows the handler only.
pub type ModHandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A provider of extra modulations (the extism mods host).
pub trait ModHandler: Send + Sync {
    /// The modulation names this handler serves (matched with
    /// [`mod_matches`](resonator_protocol::mod_matches)).
    fn mods(&self) -> Vec<String>;

    /// Serves one request. Every response frame (including Denied and
    /// Error outcomes) goes through `frames`; the pipeline forwards them
    /// to the stream and stops the handler by dropping the receiver when
    /// the client goes away.
    ///
    /// By the time this runs the node has already admitted the peer and
    /// carried the `mod:<name>` action through the chain, unless
    /// [`ModHandler::self_gated`] says otherwise.
    fn handle(
        &self,
        peer: String,
        request: Request,
        frames: mpsc::Sender<EnvelopeObject>,
    ) -> ModHandlerFuture<'_>;

    /// Whether this handler runs the peer gate and the chain itself.
    ///
    /// Default `false`: the node gates every mod request the same way it
    /// gates sql, sparql, chat and media, so a handler cannot be reached
    /// by a peer that is not in `_peers` or not allowed `mod:<name>`.
    ///
    /// Return `true` only when the handler must own the `_audit` row —
    /// the extism host does, because it updates that row with the frame
    /// and byte counts once the mod has run, and the id never crosses
    /// this trait. A handler that answers `true` and then does not gate
    /// is reachable by any peer that completes the handshake.
    fn self_gated(&self) -> bool {
        false
    }
}
