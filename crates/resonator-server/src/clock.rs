//! Time source, injected so that tests are deterministic.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Supplies the current time in milliseconds since the epoch.
pub trait Clock: Send + Sync + 'static {
    fn now_millis(&self) -> i64;
}

/// The system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    pub fn new(now_millis: i64) -> Self {
        ManualClock {
            now: AtomicI64::new(now_millis),
        }
    }

    pub fn set(&self, now_millis: i64) {
        self.now.store(now_millis, Ordering::SeqCst);
    }

    /// Moves the clock forward and returns the new time.
    pub fn advance(&self, millis: i64) -> i64 {
        self.now.fetch_add(millis, Ordering::SeqCst) + millis
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}
