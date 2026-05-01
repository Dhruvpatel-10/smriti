use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Registry that tracks background post-create tasks and periodic loops.
///
/// All spawns go through this registry so that shutdown can drain them within
/// a bounded window instead of silently abandoning in-flight work.
pub struct TaskRegistry {
    joinset: Arc<Mutex<JoinSet<()>>>,
    cancel: CancellationToken,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            joinset: Arc::new(Mutex::new(JoinSet::new())),
            cancel: CancellationToken::new(),
        }
    }

    /// Spawn a future into the registry. The task is tracked until it completes
    /// or the drain window expires.
    pub async fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.joinset.lock().await.spawn(fut);
    }

    /// Return a clone of the cancellation token for passing into periodic loops.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Cancel all registered tasks, then wait up to `drain_secs` seconds for
    /// them to complete. Tasks still running after the timeout are logged
    /// (count only — no content) and abandoned.
    ///
    /// Takes `&self` so it works correctly even when other `Arc<TaskRegistry>`
    /// clones exist (e.g. held by `KbServer` or an in-flight tool handler).
    /// The `JoinSet` is stored behind `Arc<Mutex<…>>` so all clones share the
    /// same task set; no `Arc::try_unwrap` is needed.
    pub async fn shutdown(&self, drain_secs: u64) {
        // Cancel first so running tasks that respect the token can exit quickly
        // before the drain window starts counting down.
        self.cancel.cancel();

        let joinset = Arc::clone(&self.joinset);
        let drain = async move {
            let mut set = joinset.lock().await;
            let mut remaining: usize = 0;
            while set.join_next().await.is_some() {
                remaining = remaining.saturating_add(1);
            }
            remaining
        };

        match tokio::time::timeout(Duration::from_secs(drain_secs), drain).await {
            Ok(drained) => {
                tracing::info!(drained, "task drain complete");
            }
            Err(_) => {
                // Drain timed out — tasks that exceed the window are abandoned.
                // Log count only (never content) per privacy invariant.
                tracing::warn!(
                    drain_secs,
                    "shutdown drain timed out — some background tasks were abandoned"
                );
            }
        }
    }
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}
