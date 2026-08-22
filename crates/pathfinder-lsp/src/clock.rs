//! Clock abstraction and utilities for LSP client time management.
//!
//! # Purpose (`LSP-B8`)
//!
//! Standard library clocks (`std::time::Instant`) query OS monotonic time and
//! cannot be paused or advanced in asynchronous tests. When integration tests
//! use `tokio::time::pause()` and `tokio::time::advance()`, `std::time::Instant`
//! does not advance, causing mixed-clock hazards (e.g. `tokio::time::sleep` returns
//! instantly while `std::time::Instant::elapsed()` reads ~0s).
//!
//! This module provides a unified [`Clock`] trait and implementations based on
//! [`tokio::time::Instant`], allowing tests to deterministically advance virtual time
//! (e.g. 60-minute idle checks or crash reprobe backoffs) without real wall-clock delays.

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

pub use tokio::time::Instant;

/// Trait abstracting monotonic time acquisition.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Return the current monotonic instant.
    fn now(&self) -> Instant;

    /// Return the duration elapsed since `since`.
    fn elapsed(&self, since: Instant) -> Duration {
        self.now().saturating_duration_since(since)
    }
}

/// Production clock backed by [`tokio::time::Instant::now()`].
///
/// In production, this queries the runtime monotonic clock.
/// In tests running under `tokio::time::pause()`, it reads Tokio's mock virtual clock,
/// which advances when `tokio::time::advance()` is called.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioClock;

/// Default clock alias for production use.
pub type DefaultClock = TokioClock;

impl Clock for TokioClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Explicit mock clock with manual time advancement.
///
/// Useful for testing components where time needs to be controlled independently
/// of the Tokio runtime scheduler.
#[derive(Debug, Clone)]
pub struct MockClock {
    current: Arc<Mutex<Instant>>,
}

impl MockClock {
    /// Create a new `MockClock` initialized to `initial`.
    #[must_use]
    pub fn new(initial: Instant) -> Self {
        Self {
            current: Arc::new(Mutex::new(initial)),
        }
    }

    /// Create a new `MockClock` initialized to the current Tokio instant.
    #[must_use]
    pub fn now_mock() -> Self {
        Self::new(Instant::now())
    }

    /// Advance the clock by `duration`.
    pub fn advance(&self, duration: Duration) {
        let mut guard = self.current.lock();
        *guard += duration;
    }

    /// Set the clock to an exact `instant`.
    pub fn set(&self, instant: Instant) {
        let mut guard = self.current.lock();
        *guard = instant;
    }
}

impl Default for MockClock {
    fn default() -> Self {
        Self::now_mock()
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        *self.current.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn test_tokio_clock_observably_advances_with_tokio_time() {
        let clock = TokioClock;
        let t0 = clock.now();

        // Advance Tokio virtual time by 60 minutes
        let one_hour = Duration::from_secs(3600);
        tokio::time::advance(one_hour).await;

        let t1 = clock.now();
        let elapsed = clock.elapsed(t0);

        assert_eq!(t1.saturating_duration_since(t0), one_hour);
        assert_eq!(elapsed, one_hour);
        assert_eq!(t0.elapsed(), one_hour);
    }

    #[test]
    fn test_mock_clock_manual_advancement() {
        let t0 = Instant::now();
        let clock = MockClock::new(t0);

        assert_eq!(clock.now(), t0);
        assert_eq!(clock.elapsed(t0), Duration::ZERO);

        let delta = Duration::from_secs(120);
        clock.advance(delta);

        assert_eq!(clock.now(), t0 + delta);
        assert_eq!(clock.elapsed(t0), delta);
    }

    #[tokio::test(start_paused = true)]
    async fn test_state_age_simulation_lsp_b8() {
        let clock = TokioClock;
        let spawned_at = clock.now();

        // Simulate advancing clock by 60 minutes
        tokio::time::advance(Duration::from_secs(3600)).await;

        // Verify state age is observably 60 minutes
        assert!(spawned_at.elapsed() >= Duration::from_secs(3600));
    }
}
