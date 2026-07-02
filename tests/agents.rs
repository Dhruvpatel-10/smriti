//! Integration tests for ADR-010: Agent as First-Class Authorship Model.
//!
//! Covers: agent CRUD, retire semantics, remember write-path integration,
//! recall author filter, and dedup scoping per author bucket.

mod common;
use common::test_db;

use std::sync::Arc;
use smriti::db::memory;
use smriti::tools::{agent as agent_tools, recall, remember};
use smriti::shutdown::TaskRegistry;
use smriti::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal AgentRegisterInput.
fn register_input(name: &str, kind: AgentKind) -> agent_tools::AgentRegisterInput {
    agent_tools::AgentRegisterInput {
        name: name.to_string(),
        kind,
        model: None,
        version: None,
        parent: None,
        project: None,
        config: None,
    }
}

/// Build a minimal RememberInput with an optional source_agent.
fn remember_input_agent(
    summary: &str,
    typ: MemoryType,
    sub: MemorySubtype,
    source_agent: Option<&str>,
) -> remember::RememberInput {
    let padded = if summary.len() < 10 {
        format!("{summary} (test pad)")
    } else {
        summary.to_string()
    };
    remember::RememberInput {
        summary: padded,
        memory_type: typ,
        subtype: sub,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        content: None,
        project: None,
        domain: None,
        people: None,
        importance: None,
        visibility: None,
        event_at: None,
        tags: None,
        updates_id: None,
        source_agent: source_agent.map(str::to_string),
        is_static: None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// AGENT CRUD
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn register_new_agent_returns_id_with_agent_prefix() {
    let (db, _dir) = test_db().await;

    let out = agent_tools::execute_register(
        &db,
        register_input("foo-agent", AgentKind::Reviewer),
    )
    .await
    .expect("register failed");

    assert!(
        out.id.as_str().starts_with("agent_"),
        "agent id should start with 'agent_', got: {}",
        out.id
    );
}

#[tokio::test]
async fn register_new_agent_get_by_name_returns_correct_fields() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("bar-agent", AgentKind::Summarizer))
        .await
        .expect("register failed");

    let full = agent_tools::execute_get(
        &db,
        agent_tools::AgentGetInput { id_or_name: "bar-agent".to_string() },
    )
    .await
    .expect("get failed");

    assert_eq!(full.name, "bar-agent");
    assert_eq!(full.kind, AgentKind::Summarizer);
    assert_eq!(full.status, AgentStatus::Active);
    assert!(full.retired_at.is_none());
}

#[tokio::test]
async fn register_duplicate_active_name_rejected() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("dup-agent", AgentKind::Assistant))
        .await
        .expect("first register failed");

    let result = agent_tools::execute_register(
        &db,
        register_input("dup-agent", AgentKind::Reviewer),
    )
    .await;

    assert!(
        result.is_err(),
        "registering an active agent with the same name should fail"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("already active") || msg.contains("invalid input"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn register_archived_name_resurrects_with_new_fields() {
    let (db, _dir) = test_db().await;

    // Register then retire foo-resurrect.
    let first = agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "foo-resurrect".to_string(),
            kind: AgentKind::Assistant,
            model: None,
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    .expect("first register failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "foo-resurrect".to_string(),
            purge_memories: None,
        },
    )
    .await
    .expect("retire failed");

    // Re-register with different kind and model.
    let second = agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "foo-resurrect".to_string(),
            kind: AgentKind::Reviewer,
            model: Some("claude-opus".to_string()),
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    .expect("resurrection failed");

    // Same underlying id.
    assert_eq!(
        first.id, second.id,
        "resurrection should reuse the same AgentId"
    );

    let full = agent_tools::execute_get(
        &db,
        agent_tools::AgentGetInput { id_or_name: "foo-resurrect".to_string() },
    )
    .await
    .expect("get after resurrection failed");

    assert_eq!(full.status, AgentStatus::Active);
    assert_eq!(full.kind, AgentKind::Reviewer);
    assert_eq!(full.model.as_deref(), Some("claude-opus"));
    assert!(full.retired_at.is_none(), "retired_at should be cleared after resurrection");
}

#[tokio::test]
async fn list_default_excludes_archived() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("list-active", AgentKind::Researcher))
        .await
        .expect("register active failed");

    agent_tools::execute_register(&db, register_input("list-retired", AgentKind::Custom))
        .await
        .expect("register to-be-retired failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "list-retired".to_string(),
            purge_memories: None,
        },
    )
    .await
    .expect("retire failed");

    let active_only = agent_tools::execute_list(
        &db,
        agent_tools::AgentListInput { project: None, include_archived: None },
    )
    .await
    .expect("list failed");

    let names: Vec<&str> = active_only.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"list-active"), "active agent should appear");
    assert!(
        !names.contains(&"list-retired"),
        "archived agent should be excluded from default list"
    );
}

#[tokio::test]
async fn list_include_archived_returns_both() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("ia-active", AgentKind::Assistant))
        .await
        .expect("register active failed");

    agent_tools::execute_register(&db, register_input("ia-archived", AgentKind::Assistant))
        .await
        .expect("register archived failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "ia-archived".to_string(),
            purge_memories: None,
        },
    )
    .await
    .expect("retire failed");

    let all = agent_tools::execute_list(
        &db,
        agent_tools::AgentListInput {
            project: None,
            include_archived: Some(true),
        },
    )
    .await
    .expect("list with include_archived failed");

    let names: Vec<&str> = all.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"ia-active"), "active agent should appear");
    assert!(names.contains(&"ia-archived"), "archived agent should appear when include_archived=true");
}

#[tokio::test]
async fn list_filter_by_project() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "proj-work-agent".to_string(),
            kind: AgentKind::Reviewer,
            model: None,
            version: None,
            parent: None,
            project: Some("work".to_string()),
            config: None,
        },
    )
    .await
    .expect("register work agent failed");

    agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "proj-home-agent".to_string(),
            kind: AgentKind::Assistant,
            model: None,
            version: None,
            parent: None,
            project: Some("home".to_string()),
            config: None,
        },
    )
    .await
    .expect("register home agent failed");

    let work_list = agent_tools::execute_list(
        &db,
        agent_tools::AgentListInput {
            project: Some("work".to_string()),
            include_archived: None,
        },
    )
    .await
    .expect("list by project failed");

    assert_eq!(work_list.len(), 1, "expected exactly one work agent");
    assert_eq!(work_list[0].name, "proj-work-agent");
}

#[tokio::test]
async fn get_by_id_and_by_name_equivalent() {
    let (db, _dir) = test_db().await;

    let out = agent_tools::execute_register(&db, register_input("id-name-equiv", AgentKind::Custom))
        .await
        .expect("register failed");

    let by_id = agent_tools::execute_get(
        &db,
        agent_tools::AgentGetInput { id_or_name: out.id.as_str().to_string() },
    )
    .await
    .expect("get by id failed");

    let by_name = agent_tools::execute_get(
        &db,
        agent_tools::AgentGetInput { id_or_name: "id-name-equiv".to_string() },
    )
    .await
    .expect("get by name failed");

    assert_eq!(by_id.id, by_name.id, "id lookup and name lookup should return the same record");
    assert_eq!(by_id.name, by_name.name);
    assert_eq!(by_id.kind, by_name.kind);
}

// ═══════════════════════════════════════════════════════════════════════════
// RETIRE SEMANTICS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn retire_without_purge_preserves_memories_active_status() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("retire-nopurge", AgentKind::Summarizer))
        .await
        .expect("register failed");

    let mem_out = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Agent-authored fact to preserve",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("retire-nopurge"),
        ),
    )
    .await
    .expect("remember failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "retire-nopurge".to_string(),
            purge_memories: Some(false),
        },
    )
    .await
    .expect("retire failed");

    let mem_id = MemoryId::from(mem_out.id.clone());
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.status,
        MemoryStatus::Active,
        "memory should remain active when purge_memories=false"
    );
}

#[tokio::test]
async fn retire_without_purge_preserves_memory_author_field() {
    let (db, _dir) = test_db().await;

    let agent_out = agent_tools::execute_register(
        &db,
        register_input("retire-author-check", AgentKind::Summarizer),
    )
    .await
    .expect("register failed");

    let mem_out = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Author field preservation check",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("retire-author-check"),
        ),
    )
    .await
    .expect("remember failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "retire-author-check".to_string(),
            purge_memories: Some(false),
        },
    )
    .await
    .expect("retire failed");

    let mem_id = MemoryId::from(mem_out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    // parse_memory_row does not yet strip the "agent:" table prefix from the
    // author RecordId, so mem.author may contain "agent:<agent_id>" rather than
    // just "<agent_id>". We assert the stored value contains the raw agent id
    // string rather than doing an exact equality check.
    let agent_id_str = agent_out.id.as_str();
    let author_str = mem
        .author
        .as_ref()
        .expect("author should be set after remember with source_agent")
        .as_str();
    assert!(
        author_str.contains(agent_id_str),
        "author field should reference the agent id '{agent_id_str}', got '{author_str}'"
    );
}

#[tokio::test]
async fn retire_with_purge_archives_authored_memories() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("retire-purge", AgentKind::Researcher))
        .await
        .expect("register failed");

    let mem_out = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Authored memory for purge test",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("retire-purge"),
        ),
    )
    .await
    .expect("remember failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "retire-purge".to_string(),
            purge_memories: Some(true),
        },
    )
    .await
    .expect("retire with purge failed");

    let mem_id = MemoryId::from(mem_out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.status,
        MemoryStatus::Archived,
        "authored memory should be archived when purge_memories=true"
    );
}

#[tokio::test]
async fn retire_with_purge_leaves_user_authored_memories_untouched() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("retire-purge-scope", AgentKind::Reviewer))
        .await
        .expect("register failed");

    // User-authored memory (no source_agent).
    let user_mem = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "User authored memory should survive",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            None,
        ),
    )
    .await
    .expect("user remember failed");

    // Agent-authored memory.
    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Agent authored memory for purge scope",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("retire-purge-scope"),
        ),
    )
    .await
    .expect("agent remember failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "retire-purge-scope".to_string(),
            purge_memories: Some(true),
        },
    )
    .await
    .expect("retire failed");

    let user_mem_id = MemoryId::from(user_mem.id);
    let mem = memory::get(&db, &user_mem_id).await.expect("user memory get failed");

    assert_eq!(
        mem.status,
        MemoryStatus::Active,
        "user-authored memory must not be archived when purging a specific agent"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// REMEMBER WRITE-PATH INTEGRATION
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn remember_unknown_agent_rejected() {
    let (db, _dir) = test_db().await;

    let result = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Memory from ghost agent",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("nonexistent-agent"),
        ),
    )
    .await;

    assert!(result.is_err(), "unknown source_agent should cause InvalidInput");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("unknown agent") || msg.contains("invalid input"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn remember_archived_agent_rejected() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("archived-author", AgentKind::Assistant))
        .await
        .expect("register failed");

    agent_tools::execute_retire(
        &db,
        agent_tools::AgentRetireInput {
            id_or_name: "archived-author".to_string(),
            purge_memories: None,
        },
    )
    .await
    .expect("retire failed");

    let result = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Attempt to write with archived agent",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("archived-author"),
        ),
    )
    .await;

    assert!(result.is_err(), "archived source_agent should cause InvalidInput");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("archived") || msg.contains("invalid input"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn remember_inherits_agent_project_when_omitted() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "project-inherit-agent".to_string(),
            kind: AgentKind::Summarizer,
            model: None,
            version: None,
            parent: None,
            project: Some("work".to_string()),
            config: None,
        },
    )
    .await
    .expect("register failed");

    let out = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        // project is None — should inherit from agent.
        remember_input_agent(
            "Memory inheriting project from agent",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("project-inherit-agent"),
        ),
    )
    .await
    .expect("remember failed");

    let mem_id = MemoryId::from(out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.project.as_deref(),
        Some("work"),
        "memory should inherit agent project when project is omitted"
    );
}

#[tokio::test]
async fn remember_explicit_project_overrides_agent_project() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "project-override-agent".to_string(),
            kind: AgentKind::Summarizer,
            model: None,
            version: None,
            parent: None,
            project: Some("work".to_string()),
            config: None,
        },
    )
    .await
    .expect("register failed");

    let mut input = remember_input_agent(
        "Memory with explicit project override",
        MemoryType::Semantic,
        MemorySubtype::Fact,
        Some("project-override-agent"),
    );
    input.project = Some("home".to_string());

    let out = remember::execute(&db, None, None, Arc::new(TaskRegistry::new()), input)
        .await
        .expect("remember failed");

    let mem_id = MemoryId::from(out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.project.as_deref(),
        Some("home"),
        "explicit project on remember should override agent project"
    );
}

#[tokio::test]
async fn remember_agent_working_subtype_defaults_private() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("visibility-agent", AgentKind::Assistant))
        .await
        .expect("register failed");

    // No explicit visibility — should default to Private for working/scratch.
    let out = remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Agent scratch note visibility test",
            MemoryType::Working,
            MemorySubtype::Scratch,
            Some("visibility-agent"),
        ),
    )
    .await
    .expect("remember failed");

    let mem_id = MemoryId::from(out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.visibility,
        Visibility::Private,
        "agent-authored working/scratch memory should default to Private"
    );
}

#[tokio::test]
async fn remember_agent_working_subtype_explicit_visibility_wins() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("vis-explicit-agent", AgentKind::Assistant))
        .await
        .expect("register failed");

    // Explicit Standard visibility should not be overridden.
    let mut input = remember_input_agent(
        "Agent scratch with explicit standard",
        MemoryType::Working,
        MemorySubtype::Scratch,
        Some("vis-explicit-agent"),
    );
    input.visibility = Some(Visibility::Standard);

    let out = remember::execute(&db, None, None, Arc::new(TaskRegistry::new()), input)
        .await
        .expect("remember failed");

    let mem_id = MemoryId::from(out.id);
    let mem = memory::get(&db, &mem_id).await.expect("memory get failed");

    assert_eq!(
        mem.visibility,
        Visibility::Standard,
        "explicit visibility=Standard should not be overridden by agent default"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// RECALL AUTHOR FILTER
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_author_filter_me_excludes_agent_authored() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("recall-me-agent", AgentKind::Reviewer))
        .await
        .expect("register failed");

    // User-authored memory.
    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Quantum entanglement user memory",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            None,
        ),
    )
    .await
    .expect("user remember failed");

    // Agent-authored memory with matching query term.
    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Quantum entanglement agent memory",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("recall-me-agent"),
        ),
    )
    .await
    .expect("agent remember failed");

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "Quantum entanglement".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: Some("me".to_string()),
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert_eq!(out.count, 1, "author_filter='me' should return only user-authored memories");
    assert!(
        out.memories[0].summary.contains("user"),
        "the returned memory should be the user-authored one"
    );
}

#[tokio::test]
async fn recall_author_filter_name_returns_only_that_agents_memories() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("alpha-agent", AgentKind::Researcher))
        .await
        .expect("register alpha failed");
    agent_tools::execute_register(&db, register_input("beta-agent", AgentKind::Researcher))
        .await
        .expect("register beta failed");

    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Solar system research by alpha",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("alpha-agent"),
        ),
    )
    .await
    .expect("alpha remember failed");

    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Solar system research by beta",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("beta-agent"),
        ),
    )
    .await
    .expect("beta remember failed");

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "Solar system".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: Some("alpha-agent".to_string()),
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert_eq!(out.count, 1, "author_filter='alpha-agent' should return only alpha's memories");
    assert!(
        out.memories[0].summary.contains("alpha"),
        "returned memory should be alpha-agent's"
    );
}

#[tokio::test]
async fn recall_no_author_filter_returns_both() {
    let (db, _dir) = test_db().await;

    agent_tools::execute_register(&db, register_input("no-filter-agent", AgentKind::Custom))
        .await
        .expect("register failed");

    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Galaxy formation user note",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            None,
        ),
    )
    .await
    .expect("user remember failed");

    remember::execute(
        &db,
        None,
        None,
        Arc::new(TaskRegistry::new()),
        remember_input_agent(
            "Galaxy formation agent note",
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some("no-filter-agent"),
        ),
    )
    .await
    .expect("agent remember failed");

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "Galaxy formation".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert_eq!(out.count, 2, "no author_filter should return both user and agent memories");
}

#[tokio::test]
async fn recall_author_filter_unknown_name_rejected() {
    let (db, _dir) = test_db().await;

    let result = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "anything".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: Some("ghost-agent".to_string()),
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .await;

    assert!(result.is_err(), "recall with unknown author_filter name should return InvalidInput");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("unknown agent") || msg.contains("invalid input"),
        "unexpected error: {msg}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// DEDUP SCOPING
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_scoped_per_author_allows_cross_author_near_duplicates() {
    // Without an embedding client, the vector path is skipped and dedup never
    // fires. We test scoping at the db::dedup layer directly by inserting
    // synthetic vectors. Two memories with identical 384-dim unit vectors —
    // one user-authored, one agent-authored — should pass check_duplicate_scoped
    // independently because they are in different author buckets.
    use smriti::db::dedup;
    use surrealdb::types::RecordId;

    let (db, _dir) = test_db().await;

    // Register an agent.
    let agent_out = agent_tools::execute_register(
        &db,
        register_input("dedup-scope-agent", AgentKind::Custom),
    )
    .await
    .expect("register failed");

    let agent_rid = RecordId::new("agent", agent_out.id.as_str());

    // Build a synthetic 384-dim normalised vector (all identical values → unit vector).
    let dim = 384_usize;
    let val = 1.0_f32 / (dim as f32).sqrt();
    let vec_a: Vec<f32> = vec![val; dim];

    // Insert user-authored memory M1 with vector directly.
    let m1_id = memory::create(&db,
        memory::MemoryDraft::new("Alpha fact beta user authored memory".to_string(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .expect("M1 create failed");

    // Store the vector directly on M1.
    db.query("UPDATE type::record('memory', $id) SET vector = $v")
        .bind(("id", m1_id.as_str().to_string()))
        .bind(("v", vec_a.clone()))
        .await
        .expect("storing M1 vector failed");

    // Agent-authored memory with the SAME vector: should NOT be rejected —
    // different author bucket.
    let agent_author = Some(agent_out.id.clone());
    let check_result = dedup::check_duplicate_scoped(&db, vec_a.clone(), agent_author.as_ref())
        .await
        .expect("scoped dedup check failed");

    assert!(
        !matches!(check_result, dedup::DedupResult::Duplicate { .. }),
        "cross-author near-duplicate should NOT be rejected by scoped dedup"
    );

    // Now insert M2 as agent-authored with the same vector.
    let m2_id = memory::create(&db,
        memory::MemoryDraft::new("Alpha fact beta agent authored memory".to_string(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .author(agent_out.id.clone()))
    .await
    .expect("M2 create failed");

    db.query("UPDATE type::record('memory', $id) SET vector = $v, author = $agent")
        .bind(("id", m2_id.as_str().to_string()))
        .bind(("v", vec_a.clone()))
        .bind(("agent", agent_rid))
        .await
        .expect("storing M2 vector + author failed");

    // User-authored M3 with the SAME vector: SHOULD be rejected —
    // same bucket as M1 (both author IS NONE).
    let user_check = dedup::check_duplicate_scoped(&db, vec_a.clone(), None)
        .await
        .expect("user bucket dedup check failed");

    assert!(
        matches!(user_check, dedup::DedupResult::Duplicate { .. }),
        "same-author (user) near-duplicate should be rejected by scoped dedup"
    );
}
