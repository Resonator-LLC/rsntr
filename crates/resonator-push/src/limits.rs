//! In-memory token buckets, keyed by client address, sender key or
//! device. Nothing is kept across restarts.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Rate;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Allows a key a request if every one of its buckets has a token left.
#[derive(Debug)]
pub struct Limiter<K> {
    rates: Vec<Rate>,
    max_keys: usize,
    buckets: Mutex<HashMap<K, Vec<Bucket>>>,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    pub fn new(rates: Vec<Rate>, max_keys: usize) -> Self {
        Limiter {
            rates,
            max_keys,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn refill(rate: &Rate, bucket: &mut Bucket, now: Instant) {
        let elapsed = now.saturating_duration_since(bucket.at).as_secs_f64();
        let every = Duration::from_millis(rate.every_ms).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed / every).min(f64::from(rate.burst));
        bucket.at = now;
    }

    /// Takes a token from each of `key`'s buckets at `now`; false (and
    /// nothing taken) if one is empty.
    pub fn check_at(&self, key: &K, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if !buckets.contains_key(key) && buckets.len() >= self.max_keys {
            // Forget keys whose buckets have filled up again.
            let rates = &self.rates;
            buckets.retain(|_, list| {
                list.iter_mut().zip(rates).any(|(bucket, rate)| {
                    Self::refill(rate, bucket, now);
                    bucket.tokens < f64::from(rate.burst)
                })
            });
            if buckets.len() >= self.max_keys {
                return false;
            }
        }
        let list = buckets.entry(key.clone()).or_insert_with(|| {
            self.rates
                .iter()
                .map(|rate| Bucket {
                    tokens: f64::from(rate.burst),
                    at: now,
                })
                .collect()
        });
        for (bucket, rate) in list.iter_mut().zip(&self.rates) {
            Self::refill(rate, bucket, now);
        }
        if list.iter().any(|bucket| bucket.tokens < 1.0) {
            return false;
        }
        for bucket in list.iter_mut() {
            bucket.tokens -= 1.0;
        }
        true
    }

    pub fn check(&self, key: &K) -> bool {
        self.check_at(key, Instant::now())
    }
}

/// Signatures seen lately, so that a wake request is not replayed.
#[derive(Debug)]
pub struct Replays {
    window: Duration,
    seen: Mutex<HashMap<[u8; 32], Instant>>,
}

impl Replays {
    pub fn new(window: Duration) -> Self {
        Replays {
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `id` is new (and remembers it).
    pub fn first_time(&self, id: [u8; 32], now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.len() > 100_000 {
            let window = self.window;
            seen.retain(|_, at| now.saturating_duration_since(*at) < window);
        }
        match seen.get(&id) {
            Some(at) if now.saturating_duration_since(*at) < self.window => false,
            _ => {
                seen.insert(id, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_then_one_per_interval_and_a_long_term_cap() {
        let limiter = Limiter::new(vec![Rate::new(3, 20_000), Rate::new(5, 120_000)], 10);
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        assert!(limiter.check_at(&"d", at(0)));
        assert!(limiter.check_at(&"d", at(0)));
        assert!(limiter.check_at(&"d", at(0)));
        assert!(!limiter.check_at(&"d", at(0)));
        assert!(!limiter.check_at(&"d", at(19)));
        // Others are not affected.
        assert!(limiter.check_at(&"e", at(19)));
        assert!(limiter.check_at(&"d", at(20)));
        assert!(!limiter.check_at(&"d", at(21)));
        assert!(limiter.check_at(&"d", at(40)));
        // Five used: the long bucket has refilled 1/3 by now.
        assert!(!limiter.check_at(&"d", at(60)));
        assert!(!limiter.check_at(&"d", at(80)));
        assert!(limiter.check_at(&"d", at(121)));
    }

    #[test]
    fn full_buckets_are_forgotten_when_keys_run_out() {
        let limiter = Limiter::new(vec![Rate::new(1, 1000)], 2);
        let start = Instant::now();
        assert!(limiter.check_at(&1, start));
        assert!(limiter.check_at(&2, start));
        // Both still empty: no room for a third key.
        assert!(!limiter.check_at(&3, start));
        assert!(limiter.check_at(&3, start + Duration::from_secs(2)));
        assert!(!limiter.check_at(&3, start + Duration::from_secs(2)));
    }

    #[test]
    fn replays_are_refused_within_the_window() {
        let replays = Replays::new(Duration::from_secs(10));
        let start = Instant::now();
        assert!(replays.first_time([1; 32], start));
        assert!(!replays.first_time([1; 32], start + Duration::from_secs(9)));
        assert!(replays.first_time([2; 32], start));
        assert!(replays.first_time([1; 32], start + Duration::from_secs(11)));
    }
}
