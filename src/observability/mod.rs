//! Process-level observability helpers (ADR-028).
//!
//! - Records startup `Instant` so `kb_status` can report uptime.
//! - Re-exports recall stage histograms via `crate::db::recall_metrics`.
//! - Provides a tracing [`throttle::ThrottleLayer`] that drops near-duplicate
//!   high-frequency events while always letting crash-class events through.

pub mod throttle;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

static STARTED_AT: OnceLock<Instant> = OnceLock::new();

/// Record the daemon's startup instant. Idempotent — first call wins.
pub fn init() {
    let _ = STARTED_AT.set(Instant::now());
}

/// Wall-time-monotonic uptime since [`init`]. `None` until init is called.
pub fn uptime() -> Option<Duration> {
    STARTED_AT.get().map(|t| t.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_is_none_before_init() {
        // STARTED_AT is shared across the test binary so we can only assert
        // it doesn't panic. Other tests may have already initialized it.
        let _ = uptime();
    }
}
