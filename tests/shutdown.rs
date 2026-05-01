//! TaskRegistry shutdown-drain tests (ADR-009 Fix 3).
//!
//! Verifies that:
//! 1. Spawned tasks that complete within the drain window are fully awaited.
//! 2. A long-running task causes shutdown to return within the bounded window,
//!    not block indefinitely.
//!
//! No DB needed — these tests operate directly on `TaskRegistry`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smriti::shutdown::TaskRegistry;
use tokio::time::{sleep, Instant};

// ═══════════════════════════════════════════════════════════════════════════
// HAPPY PATH: all tasks complete before drain expires
// ═══════════════════════════════════════════════════════════════════════════

/// Three tasks each sleep 100 ms then increment a shared counter.
/// shutdown(5) must wait for all three → counter must equal 3.
#[tokio::test]
async fn task_registry_drain_completes_all_short_tasks() {
    let registry = TaskRegistry::new();
    let counter = Arc::new(AtomicU32::new(0));

    for _ in 0..3 {
        let c = Arc::clone(&counter);
        registry
            .spawn(async move {
                sleep(Duration::from_millis(100)).await;
                c.fetch_add(1, Ordering::SeqCst);
            })
            .await;
    }

    registry.shutdown(5).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        3,
        "all 3 tasks must have completed within the 5-second drain window"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// TIMEOUT PATH: drain window expires before the long task finishes
// ═══════════════════════════════════════════════════════════════════════════

/// A task that sleeps 10 s is submitted, then shutdown(1) is called.
/// The shutdown must return within ~2 s, not block for the full 10 s.
///
/// Timing bound: we assert elapsed < 3 s to allow for scheduler jitter while
/// still being far below the 10-second task duration.
#[tokio::test]
async fn task_registry_shutdown_returns_within_drain_window() {
    let registry = TaskRegistry::new();

    // Long-running task — 10 seconds, much longer than drain window.
    registry
        .spawn(async move {
            sleep(Duration::from_secs(10)).await;
        })
        .await;

    let t0 = Instant::now();
    registry.shutdown(1).await; // 1-second drain window
    let elapsed = t0.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "shutdown must return within ~2 s when drain window is 1 s (elapsed: {elapsed:?})"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ZERO TASKS: empty registry shuts down immediately
// ═══════════════════════════════════════════════════════════════════════════

/// A registry with no spawned tasks must drain in well under 1 s.
#[tokio::test]
async fn task_registry_empty_shutdown_is_immediate() {
    let registry = TaskRegistry::new();

    let t0 = Instant::now();
    registry.shutdown(5).await;
    let elapsed = t0.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "empty registry shutdown must be near-instant (elapsed: {elapsed:?})"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// CANCELLATION TOKEN: tasks that respect the token exit early
// ═══════════════════════════════════════════════════════════════════════════

/// A task that select!s on the cancellation token exits as soon as cancel()
/// fires. The counter must still be incremented by tasks that complete first.
#[tokio::test]
async fn task_registry_cancellation_token_triggers_on_shutdown() {
    let registry = TaskRegistry::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Task that honours the cancellation token — will exit early on shutdown.
    let token = registry.cancellation_token();
    let c = Arc::clone(&counter);
    registry
        .spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    // Cancelled before natural completion — do not increment.
                }
                _ = async {
                    sleep(Duration::from_secs(30)).await;
                    c.fetch_add(1, Ordering::SeqCst);
                } => {}
            }
        })
        .await;

    let t0 = Instant::now();
    // 2-second drain; the task should exit via the cancel branch well before that.
    registry.shutdown(2).await;
    let elapsed = t0.elapsed();

    // The task was cancelled, so counter stays at 0.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "cancelled task must not increment counter"
    );

    // Shutdown should complete faster than the 30-second task duration.
    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown must complete well before 30-second task duration (elapsed: {elapsed:?})"
    );
}
