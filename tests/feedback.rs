//! B3 feedback loop: recall_event trace → kb_feedback → salience prior.

use std::sync::Arc;

use smriti::config::FusionConfig;
use smriti::db::memory;
use smriti::shutdown::TaskRegistry;
use smriti::tools::{feedback, recall, remember};
use smriti::types::*;

mod common;
use common::{remember_input, test_db};

fn recall_input(query: &str) -> recall::RecallInput {
    recall::RecallInput {
        query: query.to_string(),
        memory_type: None,
        project: None,
        domain: None,
        person: None,
        since: None,
        until: None,
        limit: Some(10),
        author_filter: None,
        min_score: None,
        channel: None,
        modality: None,
        rerank: None,
        spreading: None,
    }
}

async fn store(db: &smriti::db::client::SurrealClient, summary: &str) -> String {
    remember::execute(db, None, None, Arc::new(TaskRegistry::new()),
        remember_input(summary, MemoryType::Semantic, MemorySubtype::Fact))
        .await
        .expect("remember")
        .id
}

async fn recall_with(db: &smriti::db::client::SurrealClient, query: &str, fusion: &FusionConfig) -> recall::RecallOutput {
    recall::execute(db, None, None, recall_input(query), &Default::default(), fusion, &Default::default())
        .await
        .expect("recall")
}

#[tokio::test]
async fn feedback_requires_ids_from_the_trace_and_never_archives() {
    let (db, _dir) = test_db().await;
    let a = store(&db, "harbor market store lives on the mac studio").await;
    let other = store(&db, "unrelated note about gardening tomatoes").await;

    let out = recall_with(&db, "harbor market store", &Default::default()).await;
    assert!(out.trace_id.starts_with("trc_"));
    assert!(out.memories.iter().any(|m| m.id.as_str() == a));
    assert!(!out.memories.iter().any(|m| m.id.as_str() == other), "BM25 should not return the gardening note");

    // An id the recall did not return is rejected (citation lock).
    let stray = feedback::execute(&db, feedback::FeedbackInput {
        trace_id: Some(out.trace_id.clone()),
        memory_ids: vec![other.clone()],
        signal: feedback::FeedbackSignal::Used,
    }).await;
    assert!(stray.is_err(), "feedback on a memory outside the trace must fail");

    let used = feedback::execute(&db, feedback::FeedbackInput {
        trace_id: Some(out.trace_id.clone()),
        memory_ids: vec![a.clone()],
        signal: feedback::FeedbackSignal::Used,
    }).await.expect("used feedback");
    assert_eq!(used.applied, 1);

    let mut r = db.query("SELECT salience FROM type::record('memory', $id)").bind(("id", a.clone())).await.unwrap();
    let rows: Vec<serde_json::Value> = r.take(0).unwrap();
    assert!(rows[0]["salience"].as_f64().unwrap() > 0.9);

    // wrong: tagged disputed, still active.
    feedback::execute(&db, feedback::FeedbackInput {
        trace_id: None,
        memory_ids: vec![a.clone()],
        signal: feedback::FeedbackSignal::Wrong,
    }).await.expect("wrong feedback");
    let mem = memory::get(&db, &MemoryId::from(a.clone())).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Active);
    assert!(mem.tags.iter().any(|t| t == "disputed"));
}

#[tokio::test]
async fn salience_prior_reorders_only_when_weighted() {
    let (db, _dir) = test_db().await;
    let first = store(&db, "deploy notes for vega ops cluster alpha").await;
    let second = store(&db, "deploy notes for vega ops cluster beta").await;

    let base = recall_with(&db, "deploy notes vega ops cluster", &Default::default()).await;
    let ids: Vec<&str> = base.memories.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    let bottom = ids[1].to_string();

    for _ in 0..3 {
        feedback::execute(&db, feedback::FeedbackInput {
            trace_id: None,
            memory_ids: vec![bottom.clone()],
            signal: feedback::FeedbackSignal::Used,
        }).await.unwrap();
    }

    // Weight 0 (default): order unchanged.
    let unweighted = recall_with(&db, "deploy notes vega ops cluster", &Default::default()).await;
    assert_eq!(unweighted.memories[1].id.as_str(), bottom);

    // Positive weight: the used memory moves to the top.
    let weighted_cfg = FusionConfig { salience_weight: 0.05, ..Default::default() };
    let weighted = recall_with(&db, "deploy notes vega ops cluster", &weighted_cfg).await;
    assert_eq!(weighted.memories[0].id.as_str(), bottom);
    let _ = (first, second);
}
