//! A node with a tokio runtime of its own, driven from any thread outside
//! a runtime (the FFI, apps): every call blocks until done.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use resonator_server::config::Config;
use tokio::runtime::Runtime;

use crate::{
    BackgroundReport, ForegroundReport, LaunchOptions, LeaseState, Node, NodeError, launch_with,
};

/// Worker threads of the node's runtime: a device serves one person.
const WORKERS: usize = 2;

/// A running node and its runtime. Stopped by [`Handle::stop`] or when
/// dropped.
pub struct Handle {
    // Dropped in this order: the node (stopped first), then the runtime.
    node: Arc<Mutex<Option<Node>>>,
    runtime: Mutex<Option<Runtime>>,
    local_url: String,
    token: Option<String>,
    /// Stops the thread that restarts the node when it asks to.
    stop_restarts: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("Handle")
            .field("local_url", &self.local_url)
            .finish_non_exhaustive()
    }
}

/// Starts a node on `config` with a runtime of its own; blocks until it
/// serves. Must not be called from inside a tokio runtime.
pub fn start(config: Config) -> Result<Handle, NodeError> {
    start_with(config, LaunchOptions::default())
}

/// [`start`] with options.
pub fn start_with(config: Config, options: LaunchOptions) -> Result<Handle, NodeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .thread_name("resonator-node")
        .enable_all()
        .build()?;
    let node = runtime.block_on(launch_with(config, options))?;
    let handle = Handle {
        local_url: node.local_url(),
        token: node.token().map(str::to_owned),
        node: Arc::new(Mutex::new(Some(node))),
        runtime: Mutex::new(Some(runtime)),
        stop_restarts: Arc::new(tokio::sync::Notify::new()),
    };
    handle.supervise()?;
    Ok(handle)
}

/// Restarts the node in place (same address and token), keeping the
/// lock for the time.
fn relaunch_in(
    slot: &Mutex<Option<Node>>,
    runtime: &tokio::runtime::Handle,
) -> Result<(), NodeError> {
    let mut slot = lock(slot);
    let node = slot.take().ok_or(NodeError::Stopped)?;
    let node = runtime.block_on(node.relaunch())?;
    *slot = Some(node);
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Handle {
    /// Starts the thread that restarts the node when it asks to (after
    /// pairing or recovery from the settings).
    fn supervise(&self) -> Result<(), NodeError> {
        let runtime = lock(&self.runtime)
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or(NodeError::Stopped)?;
        let slot = Arc::clone(&self.node);
        let stop = Arc::clone(&self.stop_restarts);
        std::thread::Builder::new()
            .name("resonator-restart".to_owned())
            .spawn(move || {
                loop {
                    let Some(state) = lock(&slot).as_ref().map(|node| node.state().clone()) else {
                        return;
                    };
                    let restart = runtime.block_on(async {
                        tokio::select! {
                            () = state.restart_requested() => true,
                            () = stop.notified() => false,
                        }
                    });
                    drop(state);
                    if !restart {
                        return;
                    }
                    if let Err(error) = relaunch_in(&slot, &runtime) {
                        tracing::error!(%error, "the node could not start again");
                        return;
                    }
                }
            })?;
        Ok(())
    }

    /// Person mode: pairs this device by a pairing code or a
    /// `resonator:pair/...` link and restarts the node as a device of that person (same address and
    /// token). Blocks until done; up to `wait` for the inviting device to
    /// confirm an admin pairing.
    pub fn pair_join(
        &self,
        code_or_link: &str,
        name: &str,
        wait: Duration,
    ) -> Result<resonator_federation::control::JoinSummary, NodeError> {
        let summary = self
            .with_node(async |node| node.pair_join(code_or_link, name, wait).await)
            .ok_or(NodeError::Stopped)??;
        let runtime = lock(&self.runtime)
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or(NodeError::Stopped)?;
        relaunch_in(&self.node, &runtime)?;
        Ok(summary)
    }

    /// Person mode, an admin device: a new pairing invitation (link and
    /// code). Never log them: each lets a device in once.
    pub fn pair_invite(
        &self,
        admin: bool,
    ) -> Result<resonator_federation::pairing::NewInvitation, NodeError> {
        self.with_node(async |node| node.pair_invite(admin).await)
            .ok_or(NodeError::Stopped)?
    }

    /// Runs `f` on the node in its runtime; `None` once stopped.
    fn with_node<R>(&self, f: impl AsyncFnOnce(&Node) -> R) -> Option<R> {
        let node = lock(&self.node);
        let node = node.as_ref()?;
        let runtime = lock(&self.runtime);
        let runtime = runtime.as_ref()?;
        Some(runtime.block_on(f(node)))
    }

    /// `http://127.0.0.1:<port>` (device mode) of the node's HTTP server.
    pub fn local_url(&self) -> &str {
        &self.local_url
    }

    /// Device mode: the loopback sign-in token. Never log it.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Device mode: `<local_url>/?token=<token>`, the page to load in the
    /// WebView. Never log it.
    pub fn signed_in_url(&self) -> Option<String> {
        self.token
            .as_ref()
            .map(|token| format!("{}/?token={token}", self.local_url))
    }

    /// Whether the node runs (not stopped).
    pub fn is_running(&self) -> bool {
        lock(&self.node).is_some()
    }

    /// Person mode: the host lease as this device sees it.
    pub fn lease_state(&self) -> Option<LeaseState> {
        lock(&self.node).as_ref().and_then(Node::lease_state)
    }

    /// See [`Node::set_push_token`].
    pub fn set_push_token(&self, token: Option<&crate::push::PushToken>) -> Result<(), NodeError> {
        lock(&self.node)
            .as_ref()
            .ok_or(NodeError::Stopped)?
            .set_push_token(token)
    }

    /// See [`Node::enter_background`].
    pub fn enter_background(&self, budget: Duration) -> Result<BackgroundReport, NodeError> {
        self.with_node(async |node| node.enter_background(budget).await)
            .ok_or(NodeError::Stopped)
    }

    /// See [`Node::enter_foreground`].
    pub fn enter_foreground(&self, budget: Duration) -> Result<ForegroundReport, NodeError> {
        self.with_node(async |node| node.enter_foreground(budget).await)
            .ok_or(NodeError::Stopped)
    }

    /// Stops the node (federation, agents, servers; the database flushed
    /// and closed) and its runtime. Later calls do nothing.
    pub fn stop(&self) {
        self.stop_restarts.notify_one();
        let node = lock(&self.node).take();
        let runtime = lock(&self.runtime).take();
        if let Some(runtime) = runtime {
            if let Some(node) = node {
                runtime.block_on(node.shutdown());
            }
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}
