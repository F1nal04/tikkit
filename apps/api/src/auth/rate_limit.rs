//! In-memory token-bucket rate limiting for abuse-sensitive endpoints
//! (login, first setup). State is per process: it resets on restart and is
//! not shared between replicas, which is acceptable for the single-instance
//! deployment model.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Keys are pruned once the map grows beyond this many entries.
const PRUNE_THRESHOLD: usize = 10_000;

/// A keyed token bucket: each key may make `capacity` requests in a burst,
/// refilled continuously at `capacity` per `period`.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: f64,
    per_second: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl RateLimiter {
    pub fn new(capacity: u32, period: Duration) -> Self {
        let capacity = f64::from(capacity.max(1));
        Self {
            capacity,
            per_second: capacity / period.as_secs_f64().max(0.001),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key`. On `Err(secs)` the caller should answer
    /// `429` with `Retry-After: secs`.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &str, now: Instant) -> Result<(), u64> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > PRUNE_THRESHOLD {
            let (capacity, rate) = (self.capacity, self.per_second);
            buckets.retain(|_, b| {
                b.tokens + now.saturating_duration_since(b.updated).as_secs_f64() * rate < capacity
            });
        }
        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: self.capacity,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_second).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) / self.per_second;
            Err(wait.ceil().max(1.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst_then_limits_and_refills() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limiter.check_at("a", t0).is_ok());
        }
        let retry = limiter.check_at("a", t0).unwrap_err();
        assert!((1..=20).contains(&retry), "{retry}");
        assert!(limiter.check_at("b", t0).is_ok(), "keys are independent");
        assert!(limiter.check_at("a", t0 + Duration::from_secs(20)).is_ok());
        assert!(limiter.check_at("a", t0 + Duration::from_secs(20)).is_err());
    }
}
