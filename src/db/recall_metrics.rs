//! Per-stage latency histograms for the recall pipeline (ADR-028, Step 4).
//!
//! Each stage of `db::search::recall` records its own duration into a fixed-
//! size HDR histogram. `kb_status(include_diagnostics=true)` snapshots and
//! resets the histograms so each `kb_status` call returns percentiles for the
//! interval since the last call — no rolling-window machinery needed.
//!
//! Cost model: ~1µs per record() (lock + histogram increment), bounded ~1MB
//! total resident memory across all stages. Acceptable for a personal-use
//! single-host daemon.
//!
//! Initialised exactly once at daemon startup via [`init`]. Recorders that
//! fire before init are a no-op — tests don't need to set this up.

use std::sync::OnceLock;
use std::time::Duration;

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stage tags. Match the `// ── Step N` markers in `db::search::recall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStage {
    Bm25,
    Vector,
    Fusion,
    Mmr,
    Rerank,
    SpreadingActivation,
}

/// Holds one HDR histogram per stage. All histograms record microseconds with
/// 3 significant digits and a 60s ceiling — recall stages that exceed 60s are
/// already broken, the saturating-record below truncates rather than panics.
pub struct RecallStageMetrics {
    bm25: Mutex<Histogram<u64>>,
    vector: Mutex<Histogram<u64>>,
    fusion: Mutex<Histogram<u64>>,
    mmr: Mutex<Histogram<u64>>,
    rerank: Mutex<Histogram<u64>>,
    spreading: Mutex<Histogram<u64>>,
    sa_stats: Mutex<SpreadingActivationStats>,
}

impl RecallStageMetrics {
    fn new() -> Self {
        // 1µs..60s, 3 sig digits → ~256KB per stage on the heap.
        let h = || Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("static bounds");
        Self {
            bm25: Mutex::new(h()),
            vector: Mutex::new(h()),
            fusion: Mutex::new(h()),
            mmr: Mutex::new(h()),
            rerank: Mutex::new(h()),
            spreading: Mutex::new(h()),
            sa_stats: Mutex::new(SpreadingActivationStats::default()),
        }
    }

    fn slot(&self, stage: RecallStage) -> &Mutex<Histogram<u64>> {
        match stage {
            RecallStage::Bm25 => &self.bm25,
            RecallStage::Vector => &self.vector,
            RecallStage::Fusion => &self.fusion,
            RecallStage::Mmr => &self.mmr,
            RecallStage::Rerank => &self.rerank,
            RecallStage::SpreadingActivation => &self.spreading,
        }
    }

    /// Record one observation. `dur` clamps to [1µs, 60s].
    pub fn record(&self, stage: RecallStage, dur: Duration) {
        let micros = dur.as_micros().min(60_000_000) as u64;
        let micros = micros.max(1);
        let mut h = self.slot(stage).lock();
        // saturating_record never returns Err for a value already ≤ ceiling.
        h.saturating_record(micros);
    }

    /// Accumulate spreading activation walker stats for the interval.
    pub fn record_sa(&self, stats: SpreadingActivationStats) {
        let mut s = self.sa_stats.lock();
        s.candidates_seen += stats.candidates_seen;
        s.neighbors_added += stats.neighbors_added;
        s.cap_saturated |= stats.cap_saturated;
    }

    /// Snapshot all histograms into serializable percentiles, then reset.
    /// Returning the *interval-since-last-call* values keeps the diagnostic
    /// block stateless — no rolling buckets, no decay.
    pub fn snapshot_and_reset(&self) -> RecallStagePercentiles {
        let sa_stats = {
            let mut s = self.sa_stats.lock();
            let snap = s.clone();
            *s = SpreadingActivationStats::default();
            snap
        };
        RecallStagePercentiles {
            bm25: drain(&self.bm25),
            vector: drain(&self.vector),
            fusion: drain(&self.fusion),
            mmr: drain(&self.mmr),
            rerank: drain(&self.rerank),
            spreading_activation: drain(&self.spreading),
            sa_stats,
        }
    }
}

fn drain(slot: &Mutex<Histogram<u64>>) -> StagePercentiles {
    let mut h = slot.lock();
    let p = StagePercentiles {
        count: h.len(),
        p50_us: h.value_at_quantile(0.50),
        p95_us: h.value_at_quantile(0.95),
        p99_us: h.value_at_quantile(0.99),
        max_us: h.max(),
    };
    h.reset();
    p
}

/// Per-stage percentiles snapshot. Microseconds. `count = 0` when the stage
/// did not fire in the interval (e.g., `vector` when embedder is disabled).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct StagePercentiles {
    pub count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// Snapshot of all pipeline stages. Field names are stable wire-format — kb_status
/// emits this directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct RecallStagePercentiles {
    pub bm25: StagePercentiles,
    pub vector: StagePercentiles,
    pub fusion: StagePercentiles,
    /// MMR diversification stage (ADR-031). count=0 when MMR is disabled or skipped.
    pub mmr: StagePercentiles,
    pub rerank: StagePercentiles,
    pub spreading_activation: StagePercentiles,
    /// Rolling aggregate of walker behavior since last kb_status snapshot.
    pub sa_stats: SpreadingActivationStats,
}

/// Walker-level counters for the spreading activation stage (ADR-029, Change 3).
/// Distinguishes "cap saturated" from "walker found nothing" — previously indistinguishable.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct SpreadingActivationStats {
    pub candidates_seen: u64,
    pub neighbors_added: u64,
    pub cap_saturated: bool,
}

static METRICS: OnceLock<RecallStageMetrics> = OnceLock::new();

/// Initialise the global recorder. Idempotent — second call is a no-op.
pub fn init() {
    let _ = METRICS.set(RecallStageMetrics::new());
}

/// Record one observation if the global recorder is initialised; otherwise a
/// no-op (tests that don't call [`init`] still link).
pub fn record(stage: RecallStage, dur: Duration) {
    if let Some(m) = METRICS.get() {
        m.record(stage, dur);
    }
}

/// Record spreading activation walker stats. No-op when [`init`] was never called.
pub fn record_sa(stats: SpreadingActivationStats) {
    if let Some(m) = METRICS.get() {
        m.record_sa(stats);
    }
}

/// Snapshot+reset all stages. Returns `None` when [`init`] was never called.
pub fn snapshot_and_reset() -> Option<RecallStagePercentiles> {
    METRICS.get().map(|m| m.snapshot_and_reset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_clamps_below_one_us_to_one() {
        let m = RecallStageMetrics::new();
        m.record(RecallStage::Bm25, Duration::from_nanos(0));
        let s = m.snapshot_and_reset();
        assert_eq!(s.bm25.count, 1);
        assert_eq!(s.bm25.max_us, 1);
    }

    #[test]
    fn record_saturates_at_sixty_seconds() {
        let m = RecallStageMetrics::new();
        m.record(RecallStage::Vector, Duration::from_secs(120));
        let s = m.snapshot_and_reset();
        assert_eq!(s.vector.count, 1);
        // 3-sig-figure histograms quantize the upper bucket; HDR records 60s
        // into a bucket whose representative value is slightly above 60M µs.
        // We only need to assert that 120s did not record beyond the ceiling.
        assert!(
            s.vector.max_us >= 60_000_000 && s.vector.max_us < 61_000_000,
            "max_us={}",
            s.vector.max_us
        );
    }

    #[test]
    fn snapshot_and_reset_clears_state() {
        let m = RecallStageMetrics::new();
        for _ in 0..10 {
            m.record(RecallStage::Fusion, Duration::from_micros(100));
        }
        let first = m.snapshot_and_reset();
        assert_eq!(first.fusion.count, 10);
        let second = m.snapshot_and_reset();
        assert_eq!(second.fusion.count, 0);
        assert_eq!(second.fusion.p50_us, 0);
    }

    #[test]
    fn percentiles_track_distribution() {
        let m = RecallStageMetrics::new();
        for v in [10u64, 20, 30, 40, 50, 60, 70, 80, 90, 100] {
            m.record(RecallStage::Rerank, Duration::from_micros(v));
        }
        let s = m.snapshot_and_reset();
        assert_eq!(s.rerank.count, 10);
        // p50 around 50µs (within HDR resolution)
        assert!(s.rerank.p50_us >= 40 && s.rerank.p50_us <= 60);
        assert!(s.rerank.max_us >= 100 && s.rerank.max_us <= 101);
    }

    #[test]
    fn record_without_init_is_noop() {
        // Don't call init() — METRICS is shared across the test binary, so
        // we can only assert that calling record doesn't panic.
        record(RecallStage::Bm25, Duration::from_micros(1));
    }
}
