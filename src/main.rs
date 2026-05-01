use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, StreamableHttpServerConfig,
    session::local::LocalSessionManager,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use smriti::auth::{TokenMap, check_bearer};
use smriti::config::AppConfig;
use smriti::db::client;
use smriti::db::client::SurrealClient;
use smriti::server::KbServer;
use smriti::shutdown::TaskRegistry;
use smriti::types::AgentId;

/// Idempotently register the `auto-consolidator` system agent.
///
/// If the name already exists as an active agent, look up and return its ID —
/// the sidecar path is deterministic, so resurrection is safe. Any other error
/// propagates as a fatal startup error when auto_enabled=true, because running
/// with wrong or missing provenance is worse than refusing to start.
async fn ensure_auto_consolidator_agent(db: &SurrealClient) -> smriti::error::Result<AgentId> {
    use smriti::db::agent::{register, resolve_by_name, RegisterInput};
    use smriti::error::KbError;
    use smriti::types::AgentKind;

    let name = "auto-consolidator";
    match register(
        db,
        RegisterInput {
            name: name.to_string(),
            kind: AgentKind::Summarizer,
            model: Some("claude-sonnet-4-5".to_string()),
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    {
        Ok(id) => Ok(id),
        Err(KbError::InvalidInput(ref msg)) if msg.contains("already active") => {
            // Name conflict → look up the existing agent's ID.
            resolve_by_name(db, name)
                .await?
                .map(|a| a.id)
                .ok_or_else(|| KbError::NotFound(name.to_string()))
        }
        Err(e) => Err(e),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging to stderr (stdout reserved for MCP protocol in stdio mode)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let base_dir = std::env::var("KB_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().expect("cannot determine current directory"));

    let config_dir = std::env::var("KB_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().expect("cannot determine current directory"));

    let config = AppConfig::load(&config_dir).context("loading config")?;

    let db_path = config.db_path(&base_dir);
    let db = client::connect(&db_path).await.context("connecting to database")?;

    // CLI subcommands: --backup runs export and exits.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--backup") {
        let export_dir = config.export_dir(&base_dir);
        tokio::fs::create_dir_all(&export_dir).await?;
        let result = smriti::db::export::export_all(
            &db,
            &export_dir,
            config.export.exclude_sensitive,
        )
        .await
        .context("backup export failed")?;
        info!("backup complete: {}", result.path);
        return Ok(());
    }

    // --reextract-anchors: backfill anchor_entities for all active non-sensitive
    // memories that have not yet been processed. Admin path only — no MCP tool.
    if args.iter().any(|a| a == "--reextract-anchors") {
        if !config.llm.enabled {
            eprintln!(
                "error: LLM sidecar is disabled (llm.enabled = false in config.toml). \
                 Set llm.enabled = true and ensure sidecar/dist/index.js is built."
            );
            std::process::exit(1);
        }

        let batch_delay_ms: u64 = args
            .windows(2)
            .find(|w| w[0] == "--batch-delay-ms")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(0);

        let reextract_cancel = tokio_util::sync::CancellationToken::new();
        let (sidecar, reextract_service) =
            smriti::llm::sidecar::SidecarClient::new(config.llm.clone(), base_dir.clone(), reextract_cancel.clone())
                .await
                .context("starting LLM sidecar for reextract-anchors")?;

        // Ctrl-C handler — signal the loop to stop.
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = cancelled.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("\n[reextract-anchors] SIGINT received, stopping after current page");
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        }

        let page_size: i64 = 50;
        let mut offset: i64 = 0;
        let mut total_processed: usize = 0;
        let mut total_anchored: usize = 0;

        eprintln!("[reextract-anchors] starting backfill (page_size={page_size}, delay={batch_delay_ms}ms)");

        loop {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }

            let mut result = db
                .query(
                    "SELECT id, summary, project, domain FROM memory
                     WHERE anchor_entities IS NONE
                       AND status = 'active'
                       AND visibility != 'sensitive'
                     LIMIT $limit START $offset",
                )
                .bind(("limit", page_size))
                .bind(("offset", offset))
                .await
                .context("querying memories for reextract")?;

            let rows: Vec<serde_json::Value> = result.take(0).context("reading reextract rows")?;
            if rows.is_empty() {
                break;
            }

            for row in &rows {
                if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let id_raw = row
                    .get("id")
                    .and_then(|v| smriti::db::helpers::record_id_bare(v, "memory"))
                    .unwrap_or_default();
                if id_raw.is_empty() {
                    continue;
                }

                let memory_id = smriti::types::MemoryId::from(id_raw);
                let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let project = row.get("project").and_then(|v| v.as_str()).map(String::from);
                let domain = row
                    .get("domain")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let count = smriti::llm::extractor::extract_anchors_and_persist(
                    &db,
                    &sidecar,
                    &memory_id,
                    &summary,
                    project.as_deref(),
                    domain.as_deref(),
                )
                .await
                .unwrap_or(0);

                total_processed += 1;
                if count > 0 {
                    total_anchored += 1;
                }

                if total_processed.is_multiple_of(25) {
                    eprintln!(
                        "[reextract-anchors] processed={total_processed} anchored={total_anchored}"
                    );
                }

                if batch_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(batch_delay_ms)).await;
                }
            }

            offset += page_size;
        }

        eprintln!(
            "[reextract-anchors] done: processed={total_processed} anchored={total_anchored}"
        );
        // Shut down the supervisor and wait for it to exit before returning.
        let _ = reextract_service.shutdown(Duration::from_secs(5)).await;
        return Ok(());
    }

    // --retry-failed-extraction: re-run sidecar extraction for all memories
    // whose extraction_status is a retryable failure (anchor_failed, relation_failed,
    // both_failed, sidecar_unavailable). Does not touch complete/skipped/pending.
    if args.iter().any(|a| a == "--retry-failed-extraction") {
        if !config.llm.enabled {
            eprintln!(
                "error: LLM sidecar is disabled (llm.enabled = false in config.toml). \
                 Set llm.enabled = true and ensure sidecar/dist/index.js is built."
            );
            std::process::exit(1);
        }

        let batch_delay_ms: u64 = args
            .windows(2)
            .find(|w| w[0] == "--batch-delay-ms")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(0);

        let retry_cancel = tokio_util::sync::CancellationToken::new();
        let (sidecar, retry_service) =
            smriti::llm::sidecar::SidecarClient::new(config.llm.clone(), base_dir.clone(), retry_cancel.clone())
                .await
                .context("starting LLM sidecar for retry-failed-extraction")?;

        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = cancelled.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("\n[retry-failed-extraction] SIGINT received, stopping after current page");
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        }

        let page_size: i64 = 50;
        let mut offset: i64 = 0;
        let mut total_processed: usize = 0;
        let mut total_recovered: usize = 0;

        // Only retryable statuses — 'pending', 'complete', 'skipped' are excluded.
        let retryable = vec!["anchor_failed", "relation_failed", "both_failed", "sidecar_unavailable"];

        eprintln!("[retry-failed-extraction] starting retry (page_size={page_size}, delay={batch_delay_ms}ms)");

        loop {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }

            let mut result = db
                .query(
                    "SELECT id, summary, project, domain FROM memory
                     WHERE extraction_status IN $statuses
                       AND status = 'active'
                       AND visibility != 'sensitive'
                     LIMIT $limit START $offset",
                )
                .bind(("statuses", retryable.clone()))
                .bind(("limit", page_size))
                .bind(("offset", offset))
                .await
                .context("querying memories for retry-failed-extraction")?;

            let rows: Vec<serde_json::Value> = result.take(0).context("reading retry rows")?;
            if rows.is_empty() {
                break;
            }

            for row in &rows {
                if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let id_raw = row
                    .get("id")
                    .and_then(|v| smriti::db::helpers::record_id_bare(v, "memory"))
                    .unwrap_or_default();
                if id_raw.is_empty() {
                    continue;
                }

                let memory_id = smriti::types::MemoryId::from(id_raw);
                let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let project = row.get("project").and_then(|v| v.as_str()).map(String::from);
                let domain = row
                    .get("domain")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let threshold = sidecar.confidence_threshold();

                let (relation_result, anchor_result) = tokio::join!(
                    smriti::llm::extract_and_persist(
                        &db,
                        &sidecar,
                        &memory_id,
                        &summary,
                        None,
                        project.as_deref(),
                        domain.as_deref(),
                        threshold,
                    ),
                    smriti::llm::extractor::extract_anchors_and_persist(
                        &db,
                        &sidecar,
                        &memory_id,
                        &summary,
                        project.as_deref(),
                        domain.as_deref(),
                    )
                );

                let relation_ok = relation_result.is_ok();
                let anchor_ok = anchor_result.map(|_| true).unwrap_or(false);

                let new_status = match (anchor_ok, relation_ok) {
                    (true, true) => smriti::types::ExtractionStatus::Complete,
                    (false, true) => smriti::types::ExtractionStatus::AnchorFailed,
                    (true, false) => smriti::types::ExtractionStatus::RelationFailed,
                    (false, false) => smriti::types::ExtractionStatus::BothFailed,
                };

                if let Err(e) = smriti::db::memory::set_extraction_status(&db, &memory_id, new_status).await {
                    eprintln!("[retry-failed-extraction] set_extraction_status failed for {memory_id}: {e}");
                }

                total_processed += 1;
                if new_status == smriti::types::ExtractionStatus::Complete {
                    total_recovered += 1;
                }

                if total_processed.is_multiple_of(25) {
                    eprintln!(
                        "[retry-failed-extraction] processed={total_processed} recovered={total_recovered}"
                    );
                }

                if batch_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(batch_delay_ms)).await;
                }
            }

            offset += page_size;
        }

        eprintln!(
            "[retry-failed-extraction] done: processed={total_processed} recovered={total_recovered}"
        );
        // Shut down the supervisor and wait for it to exit before returning.
        let _ = retry_service.shutdown(Duration::from_secs(5)).await;
        return Ok(());
    }

    // Run initial pruning (catches anything missed since last startup)
    if let Err(e) = smriti::db::prune::run_all(&db, &config.pruning).await {
        tracing::warn!("initial pruning failed: {e}");
    }

    // Startup summary
    let active_count = smriti::db::memory::count(&db, Some(smriti::types::MemoryStatus::Active)).await.unwrap_or(0);
    let total_count = smriti::db::memory::count(&db, None).await.unwrap_or(0);
    info!(
        active_memories = active_count,
        total_memories = total_count,
        db_path = %db_path.display(),
        embeddings_enabled = config.embeddings.enabled,
        server_version = env!("CARGO_PKG_VERSION"),
        "smriti server ready"
    );

    // Shared task registry: tracks all background post-create tasks and
    // periodic loops so shutdown can drain them within a bounded window.
    let tasks = Arc::new(TaskRegistry::new());

    // Periodic pruning — hourly in long-running daemon mode.
    // In stdio mode this goroutine also runs; it adds no cost since stdio
    // processes typically exit before the first tick.
    {
        let prune_db = db.clone();
        let prune_config = config.pruning.clone();
        let cancel = tasks.cancellation_token();
        tasks.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            interval.tick().await; // skip the immediate first tick (already ran above)
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(e) = smriti::db::prune::run_all(&prune_db, &prune_config).await {
                            tracing::warn!("periodic pruning failed: {e}");
                        }
                    }
                }
            }
        }).await;
    }

    // Build an isolated SidecarClient for the consolidation loop so its circuit
    // breaker is independent from the post-create extraction path (llm_sidecar
    // in KbServer). A failure burst during consolidation will not disable
    // relation/anchor extraction for normal kb_remember writes.
    let (consolidation_sidecar, consol_sidecar_service): (
        Option<Arc<smriti::llm::sidecar::SidecarClient>>,
        Option<smriti::llm::ServiceHandle>,
    ) = if config.llm.enabled && config.consolidation.auto_enabled {
        match smriti::llm::sidecar::SidecarClient::new(config.llm.clone(), base_dir.clone(), tasks.cancellation_token()).await {
            Ok((client, service)) => {
                info!("consolidation sidecar client initialized (isolated circuit breaker)");
                (Some(client), Some(service))
            }
            Err(e) => {
                warn!(error = %e, "consolidation sidecar startup failed — auto-consolidation disabled");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Register the auto-consolidator agent when auto_enabled — idempotent.
    // Treat failure as fatal: wrong provenance is worse than not starting.
    if config.consolidation.auto_enabled && config.llm.enabled {
        match ensure_auto_consolidator_agent(&db).await {
            Ok(agent_id) => {
                info!(agent_id = %agent_id, "auto-consolidator agent ready");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "auto-consolidation enabled but agent registration failed: {e}"
                ));
            }
        }
    }

    // Periodic consolidation loop.
    //
    // When auto_enabled=false (default): runs find_candidates for observability
    // only ("find_candidates only" mode — no merges).
    // When auto_enabled=true + sidecar available: full LLM-driven merge loop.
    {
        use smriti::db::consolidation::{consolidate_atomic, fetch_memories_by_ids, ConsolidateParams};
        use smriti::db::consolidation_ledger::{cluster_hash, check_cooldown, count_attempts_last_24h, record_attempt, AttemptOutcome};
        use smriti::llm::sidecar::SynthesizeConsolidationParams;
        use smriti::types::Visibility;

        let consol_db = db.clone();
        let interval_hours = config.pruning.consolidation_interval_hours;
        let consol_sidecar = consolidation_sidecar.clone();
        let consol_cfg = config.consolidation.clone();
        let alert_cfg = config.alerts.clone();
        let md_cfg = config.markdown.clone();

        // Build embedder for the consolidation loop — reuses the same backend
        // as the main server but is a separate Arc so it can be moved into the
        // spawned task without touching KbServer.
        let consol_embedder: Option<Arc<dyn smriti::db::embeddings::Embedder>> =
            if config.embeddings.enabled {
                smriti::db::embeddings::build_embedder(&config.embeddings)
                    .ok()
                    .map(|e| e as Arc<dyn smriti::db::embeddings::Embedder>)
            } else {
                None
            };

        let cancel = tasks.cancellation_token();
        let md_tasks = tasks.clone();
        tasks.spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(interval_hours * 3600));
            interval.tick().await; // skip immediate first tick

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if !consol_cfg.auto_enabled || consol_sidecar.is_none() {
                            // Observability-only mode: surface candidate count but do not merge.
                            tracing::info!("auto-consolidation: disabled — find_candidates only");
                            match smriti::db::consolidation::find_candidates(&consol_db, 10, None, None).await {
                                Ok(candidates) if !candidates.is_empty() => {
                                    tracing::info!(
                                        count = candidates.len(),
                                        "consolidation candidates found — use kb_consolidation_candidates to review"
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!("consolidation check failed: {e}"),
                            }
                            continue;
                        }

                        tracing::info!("auto-consolidation: enabled");
                        let Some(sidecar) = consol_sidecar.as_ref() else { continue; };
                        let cooldown_days = consol_cfg.cooldown_days;
                        let overfetch = (consol_cfg.max_per_tick * 3) as usize;

                        let candidates = match smriti::db::consolidation::find_candidates(
                            &consol_db,
                            overfetch,
                            Some(consol_cfg.min_age_days),
                            Some(consol_cfg.max_cluster_size),
                        ).await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("auto-consolidation: find_candidates failed: {e}");
                                continue;
                            }
                        };

                        tracing::debug!(count = candidates.len(), "auto-consolidation: candidate batch fetched");

                        let mut merged_count: u32 = 0;
                        for candidate in &candidates {
                            if merged_count >= consol_cfg.max_per_tick {
                                break;
                            }

                            let ids = match candidate.memory_ids() {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!("auto-consolidation: could not parse candidate ids: {e}");
                                    continue;
                                }
                            };

                            let hash = cluster_hash(&ids);

                            // Skip clusters in cooldown (already evaluated recently).
                            match check_cooldown(&consol_db, &hash).await {
                                Ok(Some(_skip_until)) => {
                                    tracing::debug!(cluster_hash = %hash, "auto-consolidation: cluster in cooldown, skipping");
                                    continue;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(cluster_hash = %hash, "auto-consolidation: cooldown check failed: {e}");
                                    continue;
                                }
                            }

                            // Re-fetch source memories — visibility may have changed
                            // since find_candidates ran (defense-in-depth).
                            let sources = match fetch_memories_by_ids(&consol_db, &ids).await {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::warn!(cluster_hash = %hash, "auto-consolidation: fetch_memories_by_ids failed: {e}");
                                    continue;
                                }
                            };

                            // Detect sources silently dropped by BITEMPORAL_ACTIVE or
                            // archival between find_candidates and fetch_memories_by_ids.
                            // A shrunken cluster may no longer be cohesive — skip it.
                            if sources.len() < ids.len() {
                                tracing::warn!(
                                    cluster_hash = %hash,
                                    fetched = sources.len(),
                                    expected = ids.len(),
                                    "auto-consolidation: cluster shrank after fetch (archived/expired), skipping"
                                );
                                if let Err(e) = record_attempt(
                                    &consol_db,
                                    &hash,
                                    &ids,
                                    AttemptOutcome::DeferredSensitive,
                                    Some("cluster shrank: some memories archived or expired between candidate selection and fetch".to_string()),
                                    None,
                                    cooldown_days,
                                ).await {
                                    warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                }
                                continue;
                            }

                            // Double-check: reject any cluster containing a non-Standard memory
                            // (Private or Sensitive). find_candidates filters standard-only, but
                            // visibility can change after candidate selection.
                            if sources.iter().any(|m| m.visibility != Visibility::Standard) {
                                tracing::warn!(
                                    cluster_hash = %hash,
                                    "auto-consolidation: cluster contains non-standard visibility memory, deferring"
                                );
                                if let Err(e) = record_attempt(
                                    &consol_db,
                                    &hash,
                                    &ids,
                                    AttemptOutcome::DeferredSensitive,
                                    Some("contains private or sensitive memory".to_string()),
                                    None,
                                    cooldown_days,
                                ).await {
                                    warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                }
                                continue;
                            }

                            // Daily budget cap — guard before every sidecar call so a
                            // large candidate batch on the first tick cannot exhaust the
                            // rolling 24h allowance.
                            match count_attempts_last_24h(&consol_db).await {
                                Ok(n) if n >= consol_cfg.max_attempts_per_day => {
                                    tracing::info!(
                                        attempts_today = n,
                                        max_attempts_per_day = consol_cfg.max_attempts_per_day,
                                        "auto-consolidation: daily budget exhausted, stopping tick"
                                    );
                                    break;
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        "auto-consolidation: could not check daily budget: {e}; skipping cluster"
                                    );
                                    continue;
                                }
                            }

                            // Determine subtype from first source for sidecar context.
                            let subtype_str = sources
                                .first()
                                .map(|m| m.subtype.as_str())
                                .unwrap_or("fact")
                                .to_string();

                            let summaries: Vec<String> =
                                sources.iter().map(|m| m.summary.clone()).collect();

                            let sidecar_result = match sidecar.synthesize_consolidation(
                                SynthesizeConsolidationParams {
                                    cluster_id: hash.clone(),
                                    summaries,
                                    subtype: subtype_str,
                                    project: candidate.project.clone(),
                                    domain: None,
                                },
                            ).await {
                                Ok(r) => r,
                                Err(e) => {
                                    warn!(cluster_hash = %hash, error = %e, "auto-consolidation: sidecar synthesize_consolidation failed");
                                    if let Err(e) = record_attempt(
                                        &consol_db,
                                        &hash,
                                        &ids,
                                        AttemptOutcome::SidecarError,
                                        Some(format!("{e:?}")),
                                        None,
                                        cooldown_days,
                                    ).await {
                                        warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                    }
                                    continue;
                                }
                            };

                            if !sidecar_result.should_merge
                                || sidecar_result.confidence < consol_cfg.min_confidence
                            {
                                tracing::debug!(
                                    cluster_hash = %hash,
                                    confidence = sidecar_result.confidence,
                                    "auto-consolidation: low confidence, deferring"
                                );
                                if let Err(e) = record_attempt(
                                    &consol_db,
                                    &hash,
                                    &ids,
                                    AttemptOutcome::DeferredLowConfidence,
                                    Some(sidecar_result.reason),
                                    None,
                                    cooldown_days,
                                ).await {
                                    warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                }
                                continue;
                            }

                            let merged_summary = match sidecar_result.merged_summary {
                                Some(s) => s,
                                None => {
                                    if let Err(e) = record_attempt(
                                        &consol_db,
                                        &hash,
                                        &ids,
                                        AttemptOutcome::MergeFailed,
                                        Some("sidecar returned should_merge=true but no merged_summary".to_string()),
                                        None,
                                        cooldown_days,
                                    ).await {
                                        warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                    }
                                    continue;
                                }
                            };

                            if merged_summary.len() < 10 || merged_summary.len() > 500 {
                                tracing::warn!(
                                    cluster_hash = %hash,
                                    len = merged_summary.len(),
                                    "auto-consolidation: merged_summary length out of bounds"
                                );
                                if let Err(e) = record_attempt(
                                    &consol_db,
                                    &hash,
                                    &ids,
                                    AttemptOutcome::MergeFailed,
                                    Some("merged_summary length out of bounds".to_string()),
                                    None,
                                    cooldown_days,
                                ).await {
                                    warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                }
                                continue;
                            }

                            // Embed the merged summary (write path → embed_document).
                            let vector = if let Some(ref emb) = consol_embedder {
                                match emb.embed_document(&merged_summary).await {
                                    Ok(v) => Some(v),
                                    Err(e) => {
                                        tracing::warn!(cluster_hash = %hash, error = ?e, "auto-consolidation: embed_document failed, proceeding without vector");
                                        None
                                    }
                                }
                            } else {
                                None
                            };

                            match consolidate_atomic(
                                &consol_db,
                                &ids,
                                ConsolidateParams {
                                    summary: merged_summary.clone(),
                                    vector,
                                    source_agent: Some("auto-consolidator".to_string()),
                                    event_at: None,
                                },
                            ).await {
                                Ok(merged_id) => {
                                    if let Err(e) = record_attempt(
                                        &consol_db,
                                        &hash,
                                        &ids,
                                        AttemptOutcome::Merged,
                                        Some(sidecar_result.reason),
                                        Some(merged_id.clone()),
                                        cooldown_days,
                                    ).await {
                                        warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                    }
                                    merged_count += 1;
                                    info!(
                                        merged_id = %merged_id,
                                        source_count = ids.len(),
                                        "auto-consolidation: cluster merged"
                                    );

                                    // Trigger markdown regeneration async — don't block the loop.
                                    if md_cfg.enabled {
                                        let md_db = consol_db.clone();
                                        let md_path = md_cfg.resolved_path();
                                        let md_alert_cfg = alert_cfg.clone();
                                        md_tasks.spawn(async move {
                                            if let Err(e) = smriti::db::markdown::regenerate_file(
                                                &md_db,
                                                &md_alert_cfg,
                                                &md_path,
                                            ).await {
                                                tracing::warn!("auto-consolidation: markdown regen failed: {e}");
                                            }
                                        }).await;
                                    }
                                }
                                Err(e) => {
                                    warn!(cluster_hash = %hash, error = ?e, "auto-consolidation: consolidate_atomic failed");
                                    if let Err(e) = record_attempt(
                                        &consol_db,
                                        &hash,
                                        &ids,
                                        AttemptOutcome::MergeFailed,
                                        Some(format!("{e:?}")),
                                        None,
                                        cooldown_days,
                                    ).await {
                                        warn!(?e, cluster_hash=%hash, "failed to record consolidation attempt");
                                    }
                                }
                            }
                        }

                        if merged_count > 0 {
                            info!(merged_count, "auto-consolidation tick complete");
                        }
                    }
                }
            }
        }).await;
    }

    // Transport selection via KB_TRANSPORT env var.
    // stdio  — default, one process per Claude session (for dev / single session)
    // http   — daemon mode, all sessions share one process via Streamable HTTP
    let transport_mode = std::env::var("KB_TRANSPORT")
        .unwrap_or_else(|_| "stdio".to_string());

    match transport_mode.as_str() {
        "http" => run_http(db, &config, &base_dir, tasks, consol_sidecar_service).await,
        _ => run_stdio(db, &config, &base_dir, tasks, consol_sidecar_service).await,
    }
}

// ── Stdio transport (one process per session) ────────────────────────────────

async fn run_stdio(
    db: smriti::db::client::SurrealClient,
    config: &AppConfig,
    base_dir: &std::path::Path,
    tasks: Arc<TaskRegistry>,
    consol_sidecar_service: Option<smriti::llm::ServiceHandle>,
) -> anyhow::Result<()> {
    info!("starting kb-server (stdio transport)");
    let (server, main_sidecar_service) = KbServer::new(db, config, base_dir, tasks.clone()).await
        .map_err(|e| anyhow::anyhow!("initializing server: {e}"))?;
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await.context("starting MCP stdio server")?;

    // Wait for stdio disconnect or ctrl_c, then drain in-flight tasks.
    tokio::select! {
        result = service.waiting() => { result?; }
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, stdio transport stopping");
        }
    }

    // 1. Drain workers — they may issue final sidecar.call() during this window.
    tasks.shutdown(5).await;

    // 2. Drain sidecar supervisors — workers are gone, no new calls coming.
    if let Some(svc) = main_sidecar_service {
        let _ = svc.shutdown(Duration::from_secs(10)).await;
    }
    if let Some(svc) = consol_sidecar_service {
        let _ = svc.shutdown(Duration::from_secs(10)).await;
    }

    Ok(())
}

// ── HTTP Streamable transport (shared daemon) ────────────────────────────────

/// Hyper service wrapper that enforces Bearer token auth on every request.
///
/// When auth is disabled (no tokens file / empty file), the inner service is
/// called directly. When enabled, a missing or invalid `Authorization` header
/// causes an immediate `401 Unauthorized` response — the MCP body is never
/// touched.
///
/// Token values are never logged — only the resolved identity name.
#[derive(Clone)]
struct AuthGuard<S> {
    inner: S,
    tokens: Option<TokenMap>,
}

impl<S, B> hyper::service::Service<Request<Incoming>> for AuthGuard<S>
where
    S: hyper::service::Service<Request<Incoming>, Response = Response<B>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B: hyper::body::Body + Send + 'static,
{
    type Response = Response<http_body_util::Either<Full<Bytes>, B>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let auth_header = req
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        match check_bearer(&self.tokens, auth_header.as_deref()) {
            Ok(_identity) => {
                let inner = self.inner.clone();
                Box::pin(async move {
                    let resp = inner.call(req).await.map_err(Into::into)?;
                    Ok(resp.map(http_body_util::Either::Right))
                })
            }
            Err(e) => {
                tracing::debug!(reason = %e, "request rejected: unauthorized");
                let body = Full::new(Bytes::from_static(b"401 Unauthorized"));
                let resp = Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("WWW-Authenticate", "Bearer realm=\"smriti\"")
                    .body(http_body_util::Either::Left(body))
                    .expect("static 401 response is always valid");
                Box::pin(std::future::ready(Ok(resp)))
            }
        }
    }
}

async fn run_http(
    db: smriti::db::client::SurrealClient,
    config: &AppConfig,
    base_dir: &std::path::Path,
    tasks: Arc<TaskRegistry>,
    consol_sidecar_service: Option<smriti::llm::ServiceHandle>,
) -> anyhow::Result<()> {
    let port = std::env::var("KB_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(47821);

    let bind_addr = format!("127.0.0.1:{port}");
    info!("starting kb-server (HTTP transport) on {bind_addr}");

    // Load bearer tokens at startup — fatal on parse error, warn-only on
    // missing file (auth-disabled mode).
    let tokens = smriti::auth::load_tokens(&config.server.auth.tokens_file)
        .map_err(|e| anyhow::anyhow!("loading auth tokens: {e}"))?;

    // KbServer is Clone — each session gets a clone that shares the same
    // underlying SurrealClient Arc (one DB connection, concurrent-safe).
    let (kb_server, main_sidecar_service) = KbServer::new(db, config, base_dir, tasks.clone()).await
        .map_err(|e| anyhow::anyhow!("initializing server: {e}"))?;

    let http_config = StreamableHttpServerConfig::default();
    let session_manager = Arc::new(LocalSessionManager::default());

    let inner = TowerToHyperService::new(StreamableHttpService::new(
        move || Ok(kb_server.clone()),
        session_manager,
        http_config,
    ));

    let service = AuthGuard { inner, tokens };

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding to {bind_addr}"))?;

    info!("kb-server HTTP listening on http://{bind_addr}/mcp");
    info!("add to ~/.claude/settings.json: \"url\": \"http://{bind_addr}/mcp\"");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!(%bind_addr, "received SIGINT, HTTP transport stopping");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, addr)) => {
                        tracing::debug!(%addr, "new connection");
                        let io = TokioIo::new(stream);
                        let svc = service.clone();
                        tokio::spawn(async move {
                            if let Err(e) = Builder::new(TokioExecutor::new())
                                .serve_connection(io, svc)
                                .await
                            {
                                tracing::warn!("connection error: {e}");
                            }
                        });
                    }
                    Err(e) => tracing::warn!("accept error: {e}"),
                }
            }
        }
    }

    // 1. Drain workers — they may issue final sidecar.call() during this window.
    tasks.shutdown(5).await;

    // 2. Drain sidecar supervisors — workers are gone, no new calls coming.
    if let Some(svc) = main_sidecar_service {
        let _ = svc.shutdown(Duration::from_secs(10)).await;
    }
    if let Some(svc) = consol_sidecar_service {
        let _ = svc.shutdown(Duration::from_secs(10)).await;
    }

    Ok(())
}
