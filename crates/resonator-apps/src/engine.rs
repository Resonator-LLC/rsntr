//! The engine: one per node, shared by every app instance.
//!
//! Cranelift compiles to native code everywhere except iOS, which forbids
//! generating code at run time; there Cranelift compiles to Pulley bytecode
//! and the Pulley interpreter runs it. A ticker thread advances the epoch
//! every [`TICK`], which is how calls get their deadlines (cheaper than
//! fuel). Compiled code is cached on disk by wasmtime's own cache, keyed by
//! the wasm and the engine settings.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use wasmtime::{Cache, CacheConfig, Config, Engine};

use crate::AppError;

pub use wasmtime::component::Component;

/// How often the epoch advances.
pub(crate) const TICK: Duration = Duration::from_millis(10);

/// How to build an [`AppEngine`].
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// Where compiled code is cached; `None` compiles every time.
    pub cache_dir: Option<PathBuf>,
    /// Run on the Pulley interpreter. Always on for iOS; elsewhere it needs
    /// the `pulley` feature and is for tests.
    pub pulley: bool,
}

/// The shared wasmtime engine and its epoch ticker.
pub struct AppEngine {
    engine: Engine,
    cache: Option<Cache>,
    pulley: bool,
    stop: Arc<AtomicBool>,
}

impl AppEngine {
    pub fn new(options: EngineOptions) -> Result<Self, AppError> {
        let engine_error = |e: wasmtime::Error| AppError::Engine(format!("{e:#}"));
        let pulley = options.pulley || cfg!(target_os = "ios");
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        if pulley {
            set_pulley(&mut config)?;
        }
        let cache = match options.cache_dir {
            Some(dir) => {
                std::fs::create_dir_all(&dir)
                    .map_err(|e| AppError::Engine(format!("cache directory: {e}")))?;
                let mut cache_config = CacheConfig::new();
                cache_config.with_directory(dir);
                Some(Cache::new(cache_config).map_err(engine_error)?)
            }
            None => None,
        };
        config.cache(cache.clone());
        let engine = Engine::new(&config).map_err(engine_error)?;
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = engine.clone();
        let stopped = Arc::clone(&stop);
        thread::Builder::new()
            .name("app-epoch".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    thread::sleep(TICK);
                    ticker.increment_epoch();
                }
            })
            .map_err(|e| AppError::Engine(format!("epoch thread: {e}")))?;
        Ok(Self {
            engine,
            cache,
            pulley,
            stop,
        })
    }

    /// Compiles (or loads from the cache) an app's component.
    pub fn compile(&self, wasm: &[u8]) -> Result<Component, AppError> {
        Component::new(&self.engine, wasm).map_err(|e| AppError::Compile(format!("{e:#}")))
    }

    /// Whether this engine runs on the Pulley interpreter.
    pub fn is_pulley(&self) -> bool {
        self.pulley
    }

    /// Compilations served from the cache so far.
    pub fn cache_hits(&self) -> usize {
        self.cache.as_ref().map_or(0, Cache::cache_hits)
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl Drop for AppEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(any(feature = "pulley", target_os = "ios"))]
fn set_pulley(config: &mut Config) -> Result<(), AppError> {
    let target = if cfg!(target_pointer_width = "64") {
        "pulley64"
    } else {
        "pulley32"
    };
    config
        .target(target)
        .map_err(|e| AppError::Engine(format!("{e:#}")))?;
    Ok(())
}

#[cfg(not(any(feature = "pulley", target_os = "ios")))]
fn set_pulley(_config: &mut Config) -> Result<(), AppError> {
    Err(AppError::Engine(
        "this build has no Pulley interpreter".into(),
    ))
}
