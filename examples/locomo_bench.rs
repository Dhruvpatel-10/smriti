//! LoCoMo retrieval bench (B0).
//!
//! Ingests every dialogue turn of LoCoMo10 as a memory through the real write
//! path (`kb_remember`), then runs each QA question through the real recall
//! path (`kb_recall`) and scores whether the cited evidence turns come back.
//! Retrieval-only: no LLM judge, so a full run costs nothing locally and only
//! embedding/rerank tokens on Voyage.
//!
//! Scales:
//!   per-conv — one fresh store per conversation (~600 turns each)
//!   pooled   — all ten conversations in one store (~5.9k turns), 10x distractors
//!
//! Usage:
//!   cargo run --release --example locomo_bench -- \
//!       --data path/to/locomo10.json [--provider local|voyage] [--scale per-conv|pooled|both] \
//!       [--convs N] [--out results/]
//!
//! Dataset: https://github.com/snap-research/locomo (CC BY-NC 4.0) — not vendored.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{NaiveDateTime, TimeZone, Utc};
use serde_json::Value;

use smriti::config::{AppConfig, EmbeddingConfig, EmbeddingProvider};
use smriti::db::client;
use smriti::db::embeddings::build_embedder;
use smriti::db::embeddings::traits::Embedder;
use smriti::db::reranker::Reranker;
use smriti::shutdown::TaskRegistry;
use smriti::tools::{recall, remember};
use smriti::types::*;

const KS: [usize; 4] = [1, 5, 10, 25];
const CATEGORY: [&str; 6] = ["?", "multi-hop", "temporal", "open-domain", "single-hop", "adversarial"];

struct Args {
    data: PathBuf,
    provider: String,
    scale: String,
    convs: usize,
    out: PathBuf,
    include_adversarial: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        data: PathBuf::new(),
        provider: "local".into(),
        scale: "both".into(),
        convs: 10,
        out: PathBuf::from("bench-results"),
        include_adversarial: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--data" => a.data = PathBuf::from(val()?),
            "--provider" => a.provider = val()?,
            "--scale" => a.scale = val()?,
            "--convs" => a.convs = val()?.parse()?,
            "--out" => a.out = PathBuf::from(val()?),
            "--include-adversarial" => a.include_adversarial = true,
            other => bail!("unknown flag {other}"),
        }
    }
    if a.data.as_os_str().is_empty() {
        bail!("--data path/to/locomo10.json is required");
    }
    Ok(a)
}

/// One LoCoMo conversation, flattened.
struct Conv {
    id: String,
    turns: Vec<Turn>,
    qa: Vec<Qa>,
}
struct Turn {
    dia_id: String,
    speaker: String,
    text: String,
    at: Option<chrono::DateTime<Utc>>,
}
struct Qa {
    question: String,
    evidence: Vec<String>,
    category: usize,
}

fn parse_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    // e.g. "1:56 pm on 8 May, 2023"
    let norm = s.trim().replace("am on", "AM on").replace("pm on", "PM on");
    NaiveDateTime::parse_from_str(&norm, "%I:%M %p on %d %B, %Y")
        .ok()
        .map(|n| Utc.from_utc_datetime(&n))
}

fn load(path: &Path, include_adversarial: bool) -> Result<Vec<Conv>> {
    let raw: Vec<Value> = serde_json::from_reader(std::fs::File::open(path)?)?;
    let mut convs = Vec::new();
    for s in raw {
        let id = s["sample_id"].as_str().unwrap_or("?").to_string();
        let c = &s["conversation"];
        let mut turns = Vec::new();
        for n in 1.. {
            let Some(sess) = c.get(format!("session_{n}")).and_then(|v| v.as_array()) else { break };
            let at = c
                .get(format!("session_{n}_date_time"))
                .and_then(|v| v.as_str())
                .and_then(parse_date);
            for t in sess {
                let mut text = t["text"].as_str().unwrap_or("").to_string();
                if let Some(cap) = t.get("blip_caption").and_then(|v| v.as_str()) {
                    text.push_str(&format!(" [shares image: {cap}]"));
                }
                turns.push(Turn {
                    dia_id: t["dia_id"].as_str().unwrap_or("").to_string(),
                    speaker: t["speaker"].as_str().unwrap_or("").to_string(),
                    text,
                    at,
                });
            }
        }
        let qa = s["qa"]
            .as_array()
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|q| {
                let category = q["category"].as_u64().unwrap_or(0) as usize;
                let evidence: Vec<String> = q["evidence"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect();
                if evidence.is_empty() || (category == 5 && !include_adversarial) {
                    return None;
                }
                Some(Qa { question: q["question"].as_str()?.to_string(), evidence, category })
            })
            .collect();
        convs.push(Conv { id, turns, qa });
    }
    Ok(convs)
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn remember_input(conv: &str, t: &Turn) -> remember::RememberInput {
    let line = format!("{}: {}", t.speaker, t.text);
    let summary = if line.chars().count() < 10 { format!("{line} (said in conversation)") } else { truncate_chars(&line, 500) };
    remember::RememberInput {
        summary,
        memory_type: MemoryType::Episodic,
        subtype: MemorySubtype::Conversation,
        channel: None,
        modality: Some(Modality::Text),
        content: (line.chars().count() > 500).then_some(line),
        project: None,
        domain: Some(vec![conv.to_string()]),
        people: Some(vec![t.speaker.clone()]),
        importance: None,
        visibility: None,
        event_at: t.at,
        tags: None,
        updates_id: None,
        source_agent: None,
        is_static: None,
    }
}

#[derive(Default)]
struct Score {
    n: usize,
    hits: [usize; KS.len()],
    evidence_recall_10: f64,
    mrr: f64,
}
impl Score {
    fn add(&mut self, ranks: &[usize], evidence: usize) {
        self.n += 1;
        let first = ranks.iter().min().copied();
        for (i, k) in KS.iter().enumerate() {
            if first.is_some_and(|r| r < *k) {
                self.hits[i] += 1;
            }
        }
        self.evidence_recall_10 += ranks.iter().filter(|r| **r < 10).count() as f64 / evidence as f64;
        if let Some(r) = first {
            self.mrr += 1.0 / (r as f64 + 1.0);
        }
    }
    fn row(&self, name: &str) -> String {
        let pct = |x: usize| 100.0 * x as f64 / self.n.max(1) as f64;
        format!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.3} |",
            self.n,
            pct(self.hits[0]),
            pct(self.hits[1]),
            pct(self.hits[2]),
            pct(self.hits[3]),
            100.0 * self.evidence_recall_10 / self.n.max(1) as f64,
            self.mrr / self.n.max(1) as f64
        )
    }
}

struct RunResult {
    by_cat: BTreeMap<usize, Score>,
    all: Score,
    latencies_ms: Vec<f64>,
    tokens_returned: Vec<usize>,
    ingest_secs: f64,
    memories: usize,
    rejected: usize,
}

async fn run(
    convs: &[&Conv],
    embedder: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<Reranker>>,
    cfg: &AppConfig,
) -> Result<RunResult> {
    let dir = tempfile::tempdir()?;
    let db = client::connect(dir.path()).await?;
    if let Some(e) = &embedder {
        smriti::db::embeddings::fingerprint::check_or_initialize(&db, &e.fingerprint()).await?;
    }
    let tasks = Arc::new(TaskRegistry::new());

    // Ingest. Dedup may fold a turn into an existing memory — map many dia_ids to one id.
    let t0 = Instant::now();
    let mut id_to_dia: HashMap<String, Vec<String>> = HashMap::new();
    let mut rejected = 0usize;
    for c in convs {
        for t in &c.turns {
            let id = match remember::execute(&db, embedder.clone(), None, tasks.clone(), remember_input(&c.id, t)).await {
                Ok(out) => out.id,
                // Dedup rejection (>0.92 cosine): the turn is folded into an existing
                // memory and its own text is lost. Count it — this is what B2 fixes.
                Err(e) if e.to_string().contains("duplicate memory detected") => {
                    rejected += 1;
                    e.to_string()
                        .rsplit("memory:")
                        .next()
                        .map(|s| s.trim().to_string())
                        .context("parsing existing id from dedup error")?
                }
                Err(e) => return Err(e).with_context(|| format!("remember {} {}", c.id, t.dia_id)),
            };
            id_to_dia.entry(id).or_default().push(format!("{}#{}", c.id, t.dia_id));
        }
    }
    tasks.shutdown(30).await; // let post-create vector stores / links land
    let ingest_secs = t0.elapsed().as_secs_f64();
    let memories = id_to_dia.len();

    let mut res = RunResult {
        by_cat: BTreeMap::new(),
        all: Score::default(),
        latencies_ms: vec![],
        tokens_returned: vec![],
        ingest_secs,
        memories,
        rejected,
    };
    for c in convs {
        for q in &c.qa {
            let gold: HashSet<String> = q.evidence.iter().map(|e| format!("{}#{}", c.id, e.trim())).collect();
            let input = recall::RecallInput {
                query: q.question.clone(),
                memory_type: None,
                project: None,
                domain: None,
                person: None,
                since: None,
                until: None,
                limit: Some(*KS.last().unwrap() as u8),
                author_filter: None,
                min_score: None,
                channel: None,
                modality: None,
                rerank: None,
                spreading: None,
            };
            let t = Instant::now();
            let out = recall::execute(
                &db,
                embedder.clone(),
                reranker.clone(),
                input,
                &cfg.search.spreading,
                &cfg.search.fusion,
                &cfg.search.mmr,
            )
            .await?;
            res.latencies_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            res.tokens_returned.push(out.memories.iter().take(10).map(|m| m.summary.len() / 4).sum());

            // Rank of each gold evidence turn (first memory containing it).
            let mut ranks = Vec::new();
            for (rank, m) in out.memories.iter().enumerate() {
                if let Some(dias) = id_to_dia.get(m.id.as_str())
                    && dias.iter().any(|d| gold.contains(d))
                {
                    ranks.push(rank);
                }
            }
            res.all.add(&ranks, gold.len());
            res.by_cat.entry(q.category).or_default().add(&ranks, gold.len());
        }
    }
    Ok(res)
}

fn pctl(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn report(name: &str, r: &mut RunResult) -> String {
    let mut s = format!(
        "### {name}\n\nmemories={} dedup-rejected={} ingest={:.0}s recall p50={:.0}ms p95={:.0}ms tokens@10 mean={:.0}\n\n",
        r.memories,
        r.rejected,
        r.ingest_secs,
        pctl(&mut r.latencies_ms, 0.5),
        pctl(&mut r.latencies_ms, 0.95),
        r.tokens_returned.iter().sum::<usize>() as f64 / r.tokens_returned.len().max(1) as f64,
    );
    s.push_str("| category | n | hit@1 | hit@5 | hit@10 | hit@25 | evidence recall@10 | MRR |\n|---|---|---|---|---|---|---|---|\n");
    for (cat, sc) in &r.by_cat {
        s.push_str(&sc.row(CATEGORY.get(*cat).copied().unwrap_or("?")));
        s.push('\n');
    }
    s.push_str(&r.all.row("**all**"));
    s.push_str("\n\n");
    s
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let base = std::env::current_dir()?;
    let mut cfg = AppConfig::load(&base).context("loading config.toml")?;

    let emb_cfg = match args.provider.as_str() {
        "local" => EmbeddingConfig {
            enabled: true,
            provider: EmbeddingProvider::Local,
            model: "bge-small-en-v1.5".into(),
            dimensions: 384,
            voyage: None,
            content_prefix_chars: cfg.embeddings.content_prefix_chars,
        },
        "voyage" => cfg.embeddings.clone(),
        "none" => EmbeddingConfig { enabled: false, ..cfg.embeddings.clone() },
        p => bail!("unknown provider {p}"),
    };
    smriti::db::validation::set_embed_content_chars(emb_cfg.content_prefix_chars);
    let embedder = if emb_cfg.enabled { Some(build_embedder(&emb_cfg)?) } else { None };
    let reranker = smriti::server::build_reranker(&embedder, &emb_cfg)?;
    cfg.embeddings = emb_cfg;

    let convs = load(&args.data, args.include_adversarial)?;
    let convs: Vec<&Conv> = convs.iter().take(args.convs).collect();
    let n_q: usize = convs.iter().map(|c| c.qa.len()).sum();
    eprintln!("loaded {} conversations, {} scored questions", convs.len(), n_q);

    let mut md = format!(
        "# LoCoMo retrieval bench\n\nprovider={} model={} rerank={} pool={} mmr={}\n\n",
        args.provider,
        embedder.as_ref().map(|e| e.fingerprint().model).unwrap_or_else(|| "bm25-only".into()),
        reranker.is_some(),
        cfg.search.fusion.pool_size,
        cfg.search.mmr.enabled,
    );

    if matches!(args.scale.as_str(), "per-conv" | "both") {
        let mut agg = RunResult { by_cat: BTreeMap::new(), all: Score::default(), latencies_ms: vec![], tokens_returned: vec![], ingest_secs: 0.0, memories: 0, rejected: 0 };
        for c in &convs {
            eprintln!("per-conv: {}", c.id);
            let r = run(&[*c], embedder.clone(), reranker.clone(), &cfg).await?;
            merge(&mut agg, r);
        }
        md.push_str(&report("scale 1x (one store per conversation)", &mut agg));
    }
    if matches!(args.scale.as_str(), "pooled" | "both") {
        eprintln!("pooled: all {} conversations in one store", convs.len());
        let mut r = run(&convs, embedder.clone(), reranker.clone(), &cfg).await?;
        md.push_str(&report(&format!("scale {}x (all conversations pooled)", convs.len()), &mut r));
    }

    std::fs::create_dir_all(&args.out)?;
    let path = args.out.join(format!("locomo-{}.md", args.provider));
    std::fs::write(&path, &md)?;
    println!("{md}");
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn merge(agg: &mut RunResult, r: RunResult) {
    fn add(a: &mut Score, b: &Score) {
        a.n += b.n;
        for i in 0..KS.len() {
            a.hits[i] += b.hits[i];
        }
        a.evidence_recall_10 += b.evidence_recall_10;
        a.mrr += b.mrr;
    }
    add(&mut agg.all, &r.all);
    for (k, v) in &r.by_cat {
        add(agg.by_cat.entry(*k).or_default(), v);
    }
    agg.latencies_ms.extend(r.latencies_ms);
    agg.tokens_returned.extend(r.tokens_returned);
    agg.ingest_secs += r.ingest_secs;
    agg.memories += r.memories;
    agg.rejected += r.rejected;
}
