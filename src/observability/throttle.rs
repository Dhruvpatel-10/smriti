//! Per-callsite tracing event throttle (ADR-028, Step 6).
//!
//! Drops repeat events that fire from the same callsite within a window —
//! protects journald rate limits when a hot loop emits the same `warn!` 100
//! times per second. Skiplist always lets crash-class messages through:
//!
//! - any event whose target starts with one of the configured prefixes
//!   (default: `sidecar`, `smriti::llm`) bypasses throttling.
//! - any event with field `event_class` whose value contains `crash` or
//!   `panic` bypasses throttling.
//!
//! Counts of suppressed events are tracked per-callsite and exposed via
//! [`ThrottleLayer::suppressed_total`] for telemetry. There is no periodic
//! flush — kb_status diagnostics callers can read the running totals.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::callsite::Identifier;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Configuration for [`ThrottleLayer`].
#[derive(Debug, Clone)]
pub struct ThrottleConfig {
    /// Window during which repeated events from the same callsite are dropped.
    pub window: Duration,
    /// Targets whose events are never throttled (prefix match).
    pub allow_target_prefixes: Vec<String>,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(500),
            allow_target_prefixes: vec![
                "sidecar".to_string(),
                "smriti::llm".to_string(),
            ],
        }
    }
}

/// A tracing Layer that suppresses near-duplicate events from the same
/// callsite firing inside the configured window.
pub struct ThrottleLayer {
    cfg: ThrottleConfig,
    state: Mutex<HashMap<Identifier, Instant>>,
    suppressed: Arc<AtomicU64>,
}

impl ThrottleLayer {
    pub fn new(cfg: ThrottleConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(HashMap::new()),
            suppressed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Total events suppressed since the layer was constructed.
    pub fn suppressed_total(&self) -> u64 {
        self.suppressed.load(Ordering::Relaxed)
    }

    /// Handle to share with telemetry surfaces (kb_status diagnostics).
    /// Reads the same counter as `suppressed_total`.
    pub fn suppressed_handle(&self) -> Arc<AtomicU64> {
        self.suppressed.clone()
    }

    fn target_in_skiplist(&self, target: &str) -> bool {
        self.cfg
            .allow_target_prefixes
            .iter()
            .any(|p| target.starts_with(p.as_str()))
    }
}

/// Visitor that scans an event's fields for an `event_class` whose stringified
/// value contains "crash" or "panic". Cheap — short-circuits on first match.
#[derive(Default)]
struct CrashClassDetector {
    is_crash: bool,
}

impl Visit for CrashClassDetector {
    fn record_str(&mut self, field: &Field, value: &str) {
        if self.is_crash {
            return;
        }
        if field.name() == "event_class" && (value.contains("crash") || value.contains("panic")) {
            self.is_crash = true;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.is_crash {
            return;
        }
        if field.name() == "event_class" {
            let s = format!("{value:?}");
            if s.contains("crash") || s.contains("panic") {
                self.is_crash = true;
            }
        }
    }
}

impl<S> Layer<S> for ThrottleLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        let metadata = event.metadata();

        // 1. Target skiplist — never throttle crash-class subsystems.
        if self.target_in_skiplist(metadata.target()) {
            return true;
        }

        // 2. Field-based bypass — explicit `event_class = "crash"` always wins.
        let mut detector = CrashClassDetector::default();
        event.record(&mut detector);
        if detector.is_crash {
            return true;
        }

        // 3. Per-callsite window. Keyed by Identifier so the same `warn!()` line
        //    is throttled, but two separate `warn!()` lines are independent.
        let id = metadata.callsite();
        let now = Instant::now();
        let mut state = self.state.lock();
        match state.get(&id) {
            Some(prev) if now.duration_since(*prev) < self.cfg.window => {
                self.suppressed.fetch_add(1, Ordering::Relaxed);
                false
            }
            _ => {
                state.insert(id, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skiplist_matches_prefix() {
        let layer = ThrottleLayer::new(ThrottleConfig::default());
        assert!(layer.target_in_skiplist("sidecar"));
        assert!(layer.target_in_skiplist("sidecar.events"));
        assert!(layer.target_in_skiplist("smriti::llm::sidecar"));
        assert!(!layer.target_in_skiplist("smriti::db"));
        assert!(!layer.target_in_skiplist("rmcp"));
    }

    #[test]
    fn crash_class_field_bypass_is_exercised_via_full_layer() {
        // The detector is private; its real exercise happens through the
        // throttle bypass test below where event_class = "crash" is recorded
        // through a real tracing Event. This test simply documents the
        // expected matching logic — the substrings the detector recognizes.
        for ok in ["crash", "panic", "crash_in_supervisor", "panic_handler_run"] {
            assert!(ok.contains("crash") || ok.contains("panic"));
        }
        for not_ok in ["ordinary_warning", "info_event", "alert_fired"] {
            assert!(!(not_ok.contains("crash") || not_ok.contains("panic")));
        }
    }

    #[test]
    fn throttle_bypasses_when_event_class_field_is_crash() {
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing::subscriber::with_default;

        #[derive(Clone)]
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriterHandle;
            fn make_writer(&'a self) -> Self::Writer {
                VecWriterHandle(self.0.clone())
            }
        }
        struct VecWriterHandle(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for VecWriterHandle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = VecWriter(buf.clone());

        let throttle = ThrottleLayer::new(ThrottleConfig {
            window: Duration::from_secs(60),
            allow_target_prefixes: vec![],
        });

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .without_time();

        let subscriber = tracing_subscriber::registry()
            .with(throttle)
            .with(fmt_layer);

        with_default(subscriber, || {
            for _ in 0..3 {
                tracing::warn!(event_class = "crash", "panic recovered");
            }
        });

        let captured = String::from_utf8(buf.lock().clone()).unwrap();
        let lines = captured
            .lines()
            .filter(|l| l.contains("panic recovered"))
            .count();
        assert_eq!(lines, 3, "crash-class events must bypass throttle; got:\n{captured}");
    }

    #[test]
    fn throttle_layer_attaches_to_subscriber_without_panic() {
        // Smoke test: the layer composes with a Registry so init_tracing()
        // builds successfully even when called without a real tracing dispatcher.
        use tracing_subscriber::layer::SubscriberExt;
        let cfg = ThrottleConfig {
            window: Duration::from_millis(50),
            allow_target_prefixes: vec!["sidecar".to_string()],
        };
        let layer = ThrottleLayer::new(cfg);
        let _registry = tracing_subscriber::registry().with(layer);
    }

    /// End-to-end behavioural test using a captured `Vec<u8>` writer and a
    /// per-test Registry. Confirms that two warn! events from the same call
    /// site within the window cause the second to be dropped.
    #[test]
    fn throttle_drops_duplicate_within_window() {
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing::subscriber::with_default;

        // A MakeWriter backed by a shared Vec<u8>.
        #[derive(Clone)]
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriterHandle;
            fn make_writer(&'a self) -> Self::Writer {
                VecWriterHandle(self.0.clone())
            }
        }
        struct VecWriterHandle(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for VecWriterHandle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = VecWriter(buf.clone());

        let throttle = ThrottleLayer::new(ThrottleConfig {
            window: Duration::from_secs(60),
            allow_target_prefixes: vec![],
        });
        let suppressed_handle = throttle.suppressed_handle();

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(false)
            .without_time();

        let subscriber = tracing_subscriber::registry()
            .with(throttle)
            .with(fmt_layer);

        with_default(subscriber, || {
            for _ in 0..5 {
                tracing::warn!("noisy event");
            }
        });

        let captured = String::from_utf8(buf.lock().clone()).unwrap();
        let lines = captured.lines().filter(|l| l.contains("noisy event")).count();
        assert_eq!(lines, 1, "only the first event should pass; got:\n{captured}");
        assert_eq!(suppressed_handle.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn throttle_lets_skiplist_targets_through() {
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing::subscriber::with_default;

        #[derive(Clone)]
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriterHandle;
            fn make_writer(&'a self) -> Self::Writer {
                VecWriterHandle(self.0.clone())
            }
        }
        struct VecWriterHandle(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for VecWriterHandle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = VecWriter(buf.clone());

        let throttle = ThrottleLayer::new(ThrottleConfig {
            window: Duration::from_secs(60),
            allow_target_prefixes: vec!["sidecar".to_string()],
        });

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .without_time();

        let subscriber = tracing_subscriber::registry()
            .with(throttle)
            .with(fmt_layer);

        with_default(subscriber, || {
            for _ in 0..5 {
                tracing::warn!(target: "sidecar", "supervisor restart");
            }
        });

        let captured = String::from_utf8(buf.lock().clone()).unwrap();
        let lines = captured.lines().filter(|l| l.contains("supervisor restart")).count();
        assert_eq!(lines, 5, "skiplisted target should never be throttled; got:\n{captured}");
    }
}
