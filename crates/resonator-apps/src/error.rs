//! Errors of the app runtime.

/// Why loading or running an app failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    #[error("bad bundle: {0}")]
    Bundle(String),
    #[error("engine: {0}")]
    Engine(String),
    #[error("compile: {0}")]
    Compile(String),
    #[error("instantiate: {0}")]
    Instantiate(String),
    /// The call ran past its deadline and was interrupted.
    #[error("deadline exceeded")]
    Timeout,
    /// The guest trapped (including running out of memory).
    #[error("trap: {0}")]
    Trap(String),
    /// An earlier call failed; the instance takes no more calls.
    #[error("instance failed: {0}")]
    Failed(String),
    /// `render` returned a node list that is not a tree.
    #[error("bad render: {0}")]
    BadRender(String),
    /// The worker is gone.
    #[error("instance stopped")]
    Stopped,
}
