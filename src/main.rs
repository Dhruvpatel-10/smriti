use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, StreamableHttpServerConfig,
    session::local::LocalSessionManager,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

use smriti::config::AppConfig;
use smriti::db::client;
use smriti::server::KbServer;

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
        "kb-server initialized"
    );

    // Periodic pruning — hourly in long-running daemon mode.
    // In stdio mode this goroutine also runs; it adds no cost since stdio
    // processes typically exit before the first tick.
    {
        let prune_db = db.clone();
        let prune_config = config.pruning.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            interval.tick().await; // skip the immediate first tick (already ran above)
            loop {
                interval.tick().await;
                if let Err(e) = smriti::db::prune::run_all(&prune_db, &prune_config).await {
                    tracing::warn!("periodic pruning failed: {e}");
                }
            }
        });
    }

    // Periodic consolidation candidate detection.
    {
        let consol_db = db.clone();
        let interval_hours = config.pruning.consolidation_interval_hours;
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(interval_hours * 3600));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                match smriti::db::consolidation::find_candidates(&consol_db, 10).await {
                    Ok(candidates) if !candidates.is_empty() => {
                        tracing::info!(
                            count = candidates.len(),
                            "consolidation candidates found — use kb_consolidation_candidates to review"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("consolidation check failed: {e}"),
                }
            }
        });
    }

    // Transport selection via KB_TRANSPORT env var.
    // stdio  — default, one process per Claude session (for dev / single session)
    // http   — daemon mode, all sessions share one process via Streamable HTTP
    let transport_mode = std::env::var("KB_TRANSPORT")
        .unwrap_or_else(|_| "stdio".to_string());

    match transport_mode.as_str() {
        "http" => run_http(db, &config, &base_dir).await,
        _ => run_stdio(db, &config, &base_dir).await,
    }
}

// ── Stdio transport (one process per session) ────────────────────────────────

async fn run_stdio(
    db: smriti::db::client::SurrealClient,
    config: &AppConfig,
    base_dir: &std::path::Path,
) -> anyhow::Result<()> {
    info!("starting kb-server (stdio transport)");
    let server = KbServer::new(db, config, base_dir);
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await.context("starting MCP stdio server")?;
    service.waiting().await?;
    Ok(())
}

// ── HTTP Streamable transport (shared daemon) ────────────────────────────────

async fn run_http(
    db: smriti::db::client::SurrealClient,
    config: &AppConfig,
    base_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let port = std::env::var("KB_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(3001);

    let bind_addr = format!("127.0.0.1:{port}");
    info!("starting kb-server (HTTP transport) on {bind_addr}");

    // KbServer is Clone — each session gets a clone that shares the same
    // underlying SurrealClient Arc (one DB connection, concurrent-safe).
    let kb_server = KbServer::new(db, config, base_dir);

    let http_config = StreamableHttpServerConfig::default();
    let session_manager = Arc::new(LocalSessionManager::default());

    let service = TowerToHyperService::new(StreamableHttpService::new(
        move || Ok(kb_server.clone()),
        session_manager,
        http_config,
    ));

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding to {bind_addr}"))?;

    info!("kb-server HTTP listening on http://{bind_addr}/mcp");
    info!("add to ~/.claude/settings.json: \"url\": \"http://{bind_addr}/mcp\"");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT, shutting down");
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

    Ok(())
}
