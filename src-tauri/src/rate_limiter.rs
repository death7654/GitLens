//! Shared request-pacing helper used by every HTTP-based `ModelProvider`
//! implementation (Gemini, OpenAI-compatible, ...), so concurrency capping
//! and per-minute spacing are implemented once instead of duplicated in
//! each provider. See `gemini_provider.rs` / `openai_provider.rs` for how
//! it's wired into an actual `call()`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, Semaphore, SemaphorePermit};
use tokio::time::Instant;

#[derive(Debug)]
pub struct RateLimiter {
    /// Bounds how many requests are in flight at once.
    concurrency: Arc<Semaphore>,
    /// Minimum gap enforced between the *start* of one request and the next.
    min_interval: Duration,
    /// Start time of the most recently dispatched request. Wrapped in an
    /// async mutex (not `std::sync::Mutex`) because we intentionally hold
    /// the lock across an `.await` while sleeping out the remaining spacing
    /// — that's what serialises concurrent callers into evenly spaced slots
    /// instead of letting them all wake up and fire at once.
    last_request_started_at: Arc<AsyncMutex<Instant>>,
}

impl RateLimiter {
    /// `max_concurrent` and `max_per_minute` are both floored to 1 so a
    /// misconfigured `0` (e.g. from a bad env var) can't produce a
    /// divide-by-zero or a permanently-stuck semaphore.
    pub fn new(max_concurrent: usize, max_per_minute: u64) -> Self {
        let max_concurrent = max_concurrent.max(1);
        let max_per_minute = max_per_minute.max(1);
        Self {
            concurrency: Arc::new(Semaphore::new(max_concurrent)),
            min_interval: Duration::from_millis(60_000 / max_per_minute),
            last_request_started_at: Arc::new(AsyncMutex::new(
                Instant::now() - Duration::from_secs(3600),
            )),
        }
    }

    #[cfg(test)]
    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        self.concurrency.available_permits()
    }

    /// Blocks until both the concurrency slot and the minimum inter-request
    /// spacing allow this call to proceed, then reserves the next slot.
    /// Hold the returned permit for the lifetime of the whole request
    /// (including any retries) so a different queued caller can't jump
    /// ahead of an in-progress backoff.
    pub async fn acquire(&self) -> SemaphorePermit<'_> {
        // `unwrap` is safe: nothing ever calls `close()` on this semaphore.
        let permit = self.concurrency.acquire().await.unwrap();

        let mut last_started = self.last_request_started_at.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last_started);
        if elapsed < self.min_interval {
            tokio::time::sleep(self.min_interval - elapsed).await;
        }
        *last_started = Instant::now();
        drop(last_started);

        permit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_spacing_from_requests_per_minute() {
        let l = RateLimiter::new(3, 30); // 30/min → one every 2000ms
        assert_eq!(l.min_interval(), Duration::from_millis(2_000));
        assert_eq!(l.available_permits(), 3);
    }

    #[test]
    fn floors_zero_inputs_to_one() {
        let l = RateLimiter::new(0, 0);
        assert_eq!(l.available_permits(), 1);
        assert_eq!(l.min_interval(), Duration::from_millis(60_000));
    }

    #[tokio::test]
    async fn acquire_spaces_out_back_to_back_calls() {
        let l = RateLimiter::new(5, 600); // 100ms spacing
        let start = Instant::now();
        {
            let _p1 = l.acquire().await;
        }
        {
            let _p2 = l.acquire().await;
        }
        assert!(start.elapsed() >= Duration::from_millis(100));
    }
}