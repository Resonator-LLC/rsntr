//! Back-off for repeated failures (sign-in attempts) and a fixed-window
//! counter (registrations), both in memory and bounded in size.

use std::collections::HashMap;

/// Settings of a [`FailureLimiter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffSettings {
    /// Failures allowed before attempts are delayed; 0 disables the
    /// limiter.
    pub max_failures: u32,
    /// The first delay, in milliseconds; doubles with every further
    /// failure.
    pub base_millis: i64,
    /// The longest delay, in milliseconds. Failures are forgotten once
    /// twice this long has passed since the last one.
    pub max_millis: i64,
}

#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    last_failure: i64,
    blocked_until: i64,
}

/// Counts failures per key and, from the `max_failures`-th on, blocks the
/// key for an exponentially growing time.
#[derive(Debug)]
pub struct FailureLimiter {
    settings: BackoffSettings,
    entries: HashMap<String, Failures>,
    capacity: usize,
}

impl FailureLimiter {
    /// Keys tracked at most; beyond that the stalest are dropped.
    pub const DEFAULT_CAPACITY: usize = 100_000;

    pub fn new(settings: BackoffSettings) -> Self {
        Self::with_capacity(settings, Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(settings: BackoffSettings, capacity: usize) -> Self {
        FailureLimiter {
            settings,
            entries: HashMap::new(),
            capacity: capacity.max(1),
        }
    }

    fn expired(&self, entry: &Failures, now: i64) -> bool {
        Self::is_expired(&self.settings, entry, now)
    }

    /// An entry is forgotten once it blocks nothing and its last failure
    /// is twice the longest delay ago.
    fn is_expired(settings: &BackoffSettings, entry: &Failures, now: i64) -> bool {
        now >= entry.blocked_until
            && now - entry.last_failure >= settings.max_millis.saturating_mul(2)
    }

    /// `Ok` if the key may try now, otherwise the milliseconds to wait.
    pub fn check(&self, key: &str, now: i64) -> Result<(), i64> {
        match self.entries.get(key) {
            Some(entry) if now < entry.blocked_until => Err(entry.blocked_until - now),
            _ => Ok(()),
        }
    }

    /// Records a failed attempt.
    pub fn record_failure(&mut self, key: &str, now: i64) {
        if self.settings.max_failures == 0 {
            return;
        }
        if self.entries.len() >= self.capacity && !self.entries.contains_key(key) {
            self.prune(now);
        }
        let stale = self
            .entries
            .get(key)
            .is_some_and(|entry| self.expired(entry, now));
        if stale {
            self.entries.remove(key);
        }
        let settings = self.settings;
        let entry = self.entries.entry(key.to_owned()).or_insert(Failures {
            count: 0,
            last_failure: now,
            blocked_until: 0,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last_failure = now;
        if entry.count >= settings.max_failures {
            let doublings = (entry.count - settings.max_failures).min(40);
            let delay = settings
                .base_millis
                .max(1)
                .saturating_mul(1i64 << doublings)
                .min(settings.max_millis.max(1));
            entry.blocked_until = now.saturating_add(delay);
        }
    }

    /// Forgets a key's failures after a success.
    pub fn record_success(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Drops expired entries and, if still full, the stalest tenth.
    fn prune(&mut self, now: i64) {
        let settings = self.settings;
        self.entries
            .retain(|_, entry| !Self::is_expired(&settings, entry, now));
        if self.entries.len() >= self.capacity {
            let mut ages: Vec<i64> = self.entries.values().map(|e| e.last_failure).collect();
            ages.sort_unstable();
            let cutoff = ages[ages.len() / 10];
            self.entries.retain(|_, entry| entry.last_failure > cutoff);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Allows a number of events per key per fixed window.
#[derive(Debug)]
pub struct WindowLimiter {
    limit: u32,
    window_millis: i64,
    entries: HashMap<String, (i64, u32)>,
    capacity: usize,
}

impl WindowLimiter {
    /// `limit` 0 allows everything.
    pub fn new(limit: u32, window_millis: i64) -> Self {
        WindowLimiter {
            limit,
            window_millis: window_millis.max(1),
            entries: HashMap::new(),
            capacity: FailureLimiter::DEFAULT_CAPACITY,
        }
    }

    /// Counts an event; `Err` with the milliseconds until the window ends
    /// if the key is over its limit.
    pub fn acquire(&mut self, key: &str, now: i64) -> Result<(), i64> {
        if self.limit == 0 {
            return Ok(());
        }
        let window = self.window_millis;
        if self.entries.len() >= self.capacity && !self.entries.contains_key(key) {
            self.entries.retain(|_, (start, _)| now - *start < window);
            if self.entries.len() >= self.capacity {
                // Under a flood of distinct keys, fail closed.
                return Err(window);
            }
        }
        let entry = self.entries.entry(key.to_owned()).or_insert((now, 0));
        if now - entry.0 >= window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return Err(entry.0 + window - now);
        }
        entry.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: BackoffSettings = BackoffSettings {
        max_failures: 3,
        base_millis: 1_000,
        max_millis: 8_000,
    };

    #[test]
    fn blocks_after_max_failures_with_doubling_delay() {
        let mut limiter = FailureLimiter::new(SETTINGS);
        assert_eq!(limiter.check("k", 0), Ok(()));
        limiter.record_failure("k", 0);
        limiter.record_failure("k", 1);
        assert_eq!(limiter.check("k", 2), Ok(()));
        limiter.record_failure("k", 2);
        assert_eq!(limiter.check("k", 2), Err(1_000));
        assert_eq!(limiter.check("k", 502), Err(500));
        assert_eq!(limiter.check("k", 1_002), Ok(()));
        limiter.record_failure("k", 1_002);
        assert_eq!(limiter.check("k", 1_002), Err(2_000));
        limiter.record_failure("k", 3_002);
        assert_eq!(limiter.check("k", 3_002), Err(4_000));
        limiter.record_failure("k", 7_002);
        limiter.record_failure("k", 15_002);
        // Capped at the maximum.
        assert_eq!(limiter.check("k", 15_002), Err(8_000));
        // Other keys are unaffected.
        assert_eq!(limiter.check("other", 15_002), Ok(()));
    }

    #[test]
    fn success_and_time_forget_failures() {
        let mut limiter = FailureLimiter::new(SETTINGS);
        for _ in 0..3 {
            limiter.record_failure("k", 0);
        }
        assert!(limiter.check("k", 1).is_err());
        limiter.record_success("k");
        assert_eq!(limiter.check("k", 1), Ok(()));
        assert!(limiter.is_empty());

        limiter.record_failure("k", 0);
        limiter.record_failure("k", 0);
        // Long after the last failure the count starts over.
        limiter.record_failure("k", 16_000);
        assert_eq!(limiter.check("k", 16_000), Ok(()));
    }

    #[test]
    fn disabled_limiter_never_blocks() {
        let mut limiter = FailureLimiter::new(BackoffSettings {
            max_failures: 0,
            ..SETTINGS
        });
        for _ in 0..100 {
            limiter.record_failure("k", 0);
        }
        assert_eq!(limiter.check("k", 0), Ok(()));
        assert!(limiter.is_empty());
    }

    #[test]
    fn size_is_bounded() {
        let mut limiter = FailureLimiter::with_capacity(SETTINGS, 100);
        for i in 0..10_000 {
            limiter.record_failure(&format!("k{i}"), i);
        }
        assert!(limiter.len() <= 100);
    }

    #[test]
    fn window_limiter_counts_per_window() {
        let mut limiter = WindowLimiter::new(2, 1_000);
        assert_eq!(limiter.acquire("k", 0), Ok(()));
        assert_eq!(limiter.acquire("k", 10), Ok(()));
        assert_eq!(limiter.acquire("k", 20), Err(980));
        assert_eq!(limiter.acquire("other", 20), Ok(()));
        assert_eq!(limiter.acquire("k", 1_000), Ok(()));
        let mut unlimited = WindowLimiter::new(0, 1_000);
        for _ in 0..100 {
            assert_eq!(unlimited.acquire("k", 0), Ok(()));
        }
    }
}
