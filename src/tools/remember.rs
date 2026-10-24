use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{agent, auto_link, chunking::{self, ChunkParent}, client::SurrealClient, dedup, embeddings::Embedder, graph, memory, validation};
use crate::error::{KbError, Result};
use crate::shutdown::TaskRegistry;
use crate::types::*;

/// Build the contextual embed text for a `RememberInput`.
///
/// Must produce the identical string that the synchronous embed path would use
/// so that callers supplying a pre-computed vector (e.g. `execute_with_vector`)
/// stay aligned with the dedup and search indexes.
///
/// Call this BEFORE moving `input` into `execute_with_vector`; project
/// inheritance from agent resolution is not reflected here, so callers that
/// need agent-inherited project in the embed text should set `input.project`
/// first (the bench harness does not use agent project inheritance, so this
/// is acceptable for that use case).
pub fn build_embed_text(input: &RememberInput) -> String {
    let domain = input.domain.as_deref().unwrap_or(&[]);
    let people = input.people.as_deref().unwrap_or(&[]);
    // An invalid pairing is rejected later in execute_inner; embed with the defaults meanwhile.
    let (memory_type, subtype) = input
        .taxonomy()
        .unwrap_or((MemoryType::Semantic, MemorySubtype::Fact));
    let contextual = validation::contextual_text(
        &input.summary,
        memory_type,
        subtype,
        input.project.as_deref(),
        domain,
        people,
        input.channel,
        input.modality,
    );
    validation::with_content_prefix(contextual, input.content.as_deref())
}

impl RememberInput {
    /// Resolve the optional type/subtype hints into a valid pair.
    pub fn taxonomy(&self) -> Result<(MemoryType, MemorySubtype)> {
        match (self.memory_type, self.subtype) {
            (Some(t), Some(s)) if s.valid_for(t) => Ok((t, s)),
            (Some(t), Some(s)) => Err(crate::error::KbError::InvalidInput(format!(
                "subtype {s} is not valid for type {t}"
            ))),
            (None, Some(s)) => Ok((s.memory_type(), s)),
            (Some(t), None) => Ok((t, MemorySubtype::default_for(t))),
            (None, None) => Ok((MemoryType::Semantic, MemorySubtype::Fact)),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberInput {
    /// 50-100 token summary of the memory (REQUIRED)
    pub summary: String,
    /// Memory type hint (optional). Omitted: derived from `subtype`, else semantic.
    #[serde(rename = "type", default)]
    pub memory_type: Option<MemoryType>,
    /// Memory subtype hint (optional; must match `type` when both are given).
    /// Omitted: the type's default subtype (semantic → fact).
    #[serde(default)]
    pub subtype: Option<MemorySubtype>,
    /// Where bytes came from (slack, email, web, ...)
    pub channel: Option<Channel>,
    /// What shape they were in (text, transcript, document, code, ...)
    pub modality: Option<Modality>,
    /// Full content (stored but not returned in recall)
    pub content: Option<String>,
    /// Associated project name
    pub project: Option<String>,
    /// Life domains: health, learning, philosophy, etc.
    pub domain: Option<Vec<String>>,
    /// People involved (resolved to entity records)
    pub people: Option<Vec<String>>,
    /// Importance score 1-5 (default 3)
    pub importance: Option<u8>,
    /// Privacy: standard | private | sensitive (default standard)
    pub visibility: Option<Visibility>,
    /// When the event happened (defaults to now)
    pub event_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Tags for categorization
    pub tags: Option<Vec<String>>,
    /// ID of memory this supersedes
    pub updates_id: Option<String>,
    /// Which agent/session created this memory
    pub source_agent: Option<String>,
    /// Force-set the is_static flag. When omitted, the server derives it from
    /// subtype/importance: true for value/life_goal/preference and fact with
    /// importance >= 4. Static memories skip TTL, are surfaced first by
    /// `kb_profile`, and are excluded from importance-decay pruning.
    pub is_static: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RememberOutput {
    pub id: String,
    pub message: String,
}

#[tracing::instrument(
    name = "kb_remember",
    skip_all,
    fields(
        memory_type = ?input.memory_type,
        subtype = ?input.subtype,
        importance = input.importance,
        visibility = ?input.visibility,
        project = input.project.as_deref().unwrap_or(""),
        has_content = input.content.is_some(),
        has_updates_id = input.updates_id.is_some(),
        summary_len = input.summary.len(),
        content_len = input.content.as_ref().map(|c| c.len()).unwrap_or(0),
        people_count = input.people.as_ref().map(|p| p.len()).unwrap_or(0),
        tags_count = input.tags.as_ref().map(|t| t.len()).unwrap_or(0),
    )
)]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<dyn Embedder>>,
    llm_sidecar: Option<Arc<crate::llm::sidecar::SidecarClient>>,
    tasks: Arc<TaskRegistry>,
    mut input: RememberInput,
) -> Result<RememberOutput> {
    let resolved_author = resolve_agent(db, &mut input).await?;

    let vector = if let Some(ref client) = embedding_client {
        // Build embed text after agent resolution so project inheritance is reflected.
        let embed_text = build_embed_text_after_resolve(&input);
        match client.embed_document(&embed_text).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("embedding failed (storing without vector): {e}");
                None
            }
        }
    } else {
        None
    };

    execute_inner(db, embedding_client, llm_sidecar, tasks, input, resolved_author, vector).await
}

/// Write a pre-computed vector instead of calling the embedder.
///
/// Skips the embed step but runs everything else identically: validation,
/// dedup, create, post-create async (chunking, auto_link, agent touch,
/// sidecar typed-relation extraction, anchor entity extraction).
///
/// The supplied `vector` is used for dedup and stored on the memory record.
/// The embedder argument is still required for chunking long content — pass
/// `None` to skip chunking (acceptable for bench ingest where content is short).
pub async fn execute_with_vector(
    db: &SurrealClient,
    llm_sidecar: Option<Arc<crate::llm::sidecar::SidecarClient>>,
    tasks: Arc<TaskRegistry>,
    mut input: RememberInput,
    vector: Vec<f32>,
) -> Result<RememberOutput> {
    let resolved_author = resolve_agent(db, &mut input).await?;
    execute_inner(db, None, llm_sidecar, tasks, input, resolved_author, Some(vector)).await
}

/// Resolve `source_agent` name → `AgentId`, apply project inheritance.
///
/// Mutates `input.project` when the agent carries a project and the caller
/// left it unset. Returns the resolved `AgentId` or `None`.
async fn resolve_agent(
    db: &SurrealClient,
    input: &mut RememberInput,
) -> Result<Option<AgentId>> {
    let Some(ref name) = input.source_agent else {
        return Ok(None);
    };
    let name = name.clone();
    let ag = agent::resolve_by_name(db, &name).await?.ok_or_else(|| {
        KbError::InvalidInput(format!("unknown agent: {name}"))
    })?;
    if ag.status == AgentStatus::Archived {
        return Err(KbError::InvalidInput(format!(
            "agent {name} is archived; re-register to use"
        )));
    }
    tracing::info!(agent_id = %ag.id, agent_name = %name, "resolved author agent");
    if input.project.is_none()
        && let Some(ref agent_project) = ag.project
    {
        input.project = Some(agent_project.clone());
    }
    Ok(Some(ag.id))
}

/// Build contextual embed text from a `RememberInput` that has already had
/// agent project inheritance applied (i.e. after `resolve_agent`).
fn build_embed_text_after_resolve(input: &RememberInput) -> String {
    build_embed_text(input)
}

async fn execute_inner(
    db: &SurrealClient,
    embedding_client: Option<Arc<dyn Embedder>>,
    llm_sidecar: Option<Arc<crate::llm::sidecar::SidecarClient>>,
    tasks: Arc<TaskRegistry>,
    input: RememberInput,
    resolved_author: Option<AgentId>,
    precomputed_vector: Option<Vec<f32>>,
) -> Result<RememberOutput> {
    // ── Phase 1: Validate (synchronous, zero latency) ──────────────────

    validation::validate_summary(&input.summary)?;
    validation::validate_content(input.content.as_deref())?;

    let (memory_type, subtype) = input.taxonomy()?;

    let raw_importance = match input.importance {
        Some(v) => Importance::new(v)
            .map_err(|e| crate::error::KbError::InvalidInput(e.to_string()))?,
        None => Importance::default(),
    };
    let importance = validation::enforce_importance(subtype, raw_importance);
    let enforced_ttl = validation::ttl_duration(memory_type, subtype)
        .map(|d| chrono::Utc::now() + d);

    let people = input.people.unwrap_or_default();
    let tags = input.tags.unwrap_or_default();
    let domain = input.domain.unwrap_or_default();

    // Private-by-default for agent-authored ephemeral (working subtypes).
    // Only applies when the caller did not explicitly set visibility.
    let is_working_subtype = matches!(
        subtype,
        MemorySubtype::Priority
            | MemorySubtype::OpenQuestion
            | MemorySubtype::Scratch
            | MemorySubtype::Signal
            | MemorySubtype::Hypothesis
    );
    // Default to Private when:
    //   (a) an agent authored a working-subtype memory (ephemeral scratchpad), OR
    //   (b) memory arrived via a conversational channel (Slack, Discord, VoiceApp, SMS).
    // Explicit caller-supplied visibility always wins over both defaults.
    let visibility = if input.visibility.is_none()
        && ((resolved_author.is_some() && is_working_subtype)
            || matches!(
                input.channel,
                Some(Channel::Slack | Channel::Discord | Channel::VoiceApp | Channel::Sms)
            ))
    {
        Visibility::Private
    } else {
        input.visibility.unwrap_or_default()
    };

    // ── Phase 3: Supersede-first (atomic gate before create) ──────────
    // Supersede the old memory BEFORE creating the new one. This closes the
    // concurrency race: two concurrent calls with the same updates_id both
    // attempt the conditional UPDATE; only one sees status='active' and wins.
    // The loser gets Ok(false) here and aborts before any new record is written,
    // leaving no orphaned memories or duplicate updates edges.
    //
    // Pass the new memory's event_at as the superseder timestamp so valid_until
    // on the old memory records when the new fact became true.
    let new_event_at = input.event_at.unwrap_or_else(chrono::Utc::now);
    let superseded_old_id: Option<MemoryId> = if let Some(ref uid) = input.updates_id {
        let old_id = MemoryId::from(uid.clone());
        match memory::supersede(db, &old_id, new_event_at).await {
            Ok(true) => {
                tracing::info!(old_id = %old_id, "supersession gate passed — proceeding to create new memory");
                Some(old_id)
            }
            Ok(false) => {
                return Err(crate::error::KbError::InvalidInput(format!(
                    "target memory {old_id} not found or already superseded"
                )));
            }
            Err(e) => {
                return Err(e);
            }
        }
    } else {
        None
    };

    // ── Phase 2+4: Atomic dedup + create ──────────────────────────────
    // When a vector is available, dedup check and CREATE run inside a single
    // SurrealDB transaction. SurrealKV's single-writer serialisation ensures
    // the check-then-create is never interleaved with a concurrent writer,
    // closing the TOCTOU window that exists when the two operations are issued
    // as separate queries. A THROW inside the transaction rolls it back before
    // any record is written, so no phantom record is created on rejection.
    //
    // When no vector is available (no embedder), we fall back to a plain create
    // without dedup (no vector means nothing to compare).

    let summary_text = input.summary.clone();
    let project_clone = input.project.clone();
    // Retained for LLM extractor calls (relations + anchors) after embedding block.
    let project_for_extract = input.project.clone();
    let project_for_anchors = input.project.clone();

    // Capture fields needed for chunking before they are consumed by the create call.
    let chunk_parent: Option<ChunkParent> = match &input.content {
        Some(c) if c.len() >= chunking::CHUNK_THRESHOLD => Some(ChunkParent {
            summary: input.summary.clone(),
            content: c.clone(),
            memory_type,
            subtype,
            channel: input.channel,
            modality: input.modality,
            project: input.project.clone(),
            domain: domain.clone(),
            importance,
            visibility,
            event_at: input.event_at.unwrap_or_else(chrono::Utc::now),
            tags: tags.clone(),
            expires_at: enforced_ttl,
            source_agent: input.source_agent.clone(),
            session_id: None,
            author: resolved_author.clone(),
        }),
        _ => None,
    };

    let draft = memory::MemoryDraft::new(input.summary, memory_type, subtype, input.channel, input.modality)
        .maybe_content(input.content)
        .maybe_project(input.project)
        .domain(domain.clone())
        .people(people.clone())
        .importance(importance)
        .visibility(visibility)
        .maybe_event_at(input.event_at)
        .tags(tags)
        .maybe_expires_at(enforced_ttl)
        .maybe_source_agent(input.source_agent)
        .maybe_author(resolved_author.clone())
        .maybe_is_static(input.is_static);

    let mut warning_msg: Option<String> = None;

    let (id, precomputed_vector) = if let Some(vector) = precomputed_vector {
        // Clone before atomic call: vector still needed for store_vector_pub
        // and spawn_auto_link_with_vector after create.
        let vector_for_autolink = vector.clone();
        match memory::create_with_dedup_atomic(db, draft, vector, resolved_author.as_ref()).await {
            Ok((new_id, Some(dedup::DedupResult::Warning { ref similar_id, similarity }))) => {
                tracing::info!(closest_id = %similar_id, similarity, "dedup: near-duplicate accepted with warning");
                warning_msg = Some(format!(
                    "near-duplicate (similarity {similarity:.2}) of memory:{similar_id}"
                ));
                (new_id, Some(vector_for_autolink))
            }
            Ok((new_id, _)) => (new_id, Some(vector_for_autolink)),
            Err(create_err) => {
                // Rollback supersede on any create failure (including dedup rejection).
                if let Some(ref old_id) = superseded_old_id {
                    let new_summary_len = summary_text.len();
                    match memory::unsupersede(db, old_id).await {
                        Ok(true) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rolled back after create failure"),
                        Ok(false) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rollback matched no rows — manual check required"),
                        Err(rb_err) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rollback failed: {rb_err}"),
                    }
                }
                return Err(create_err);
            }
        }
    } else {
        // No vector — skip dedup, plain create.
        let id = match memory::create(db, draft).await {
            Ok(new_id) => new_id,
            Err(create_err) => {
                if let Some(ref old_id) = superseded_old_id {
                    let new_summary_len = summary_text.len();
                    match memory::unsupersede(db, old_id).await {
                        Ok(true) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rolled back after create failure"),
                        Ok(false) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rollback matched no rows — manual check required"),
                        Err(rb_err) => tracing::warn!(old_id = %old_id, new_summary_len, memory_type = ?memory_type, subtype = ?subtype, "supersede rollback failed: {rb_err}"),
                    }
                }
                return Err(create_err);
            }
        };
        (id, None)
    };

    // ── Phase 5: Post-create (graph edges, vector store, auto-link) ────

    // Stale-read window between memory visibility and post-create tasks
    // (vectors, edges, entity links) is accepted per ADR (drain session is
    // optional for correctness; consumers must tolerate eventual consistency
    // of post-create data). See docs/decisions/ and CLAUDE.md.

    // Link people to entity table
    for person in &people {
        graph::link_entity(db, &id, person).await?;
    }

    // Wire `assigned` edge for action items
    if matches!(subtype, MemorySubtype::ActionItem)
        && let Some(assignee) = people.first()
    {
        graph::link_assigned(db, &id, assignee).await?;
    }

    // Create the updates edge linking new memory to the (now-superseded) old memory.
    if let Some(ref old_id) = superseded_old_id {
        tracing::debug!(superseded_id = %old_id, new_id = %id, "memory superseded");
        tracing::info!(new_id = %id, old_id = %old_id, "memory superseded — creating updates edge");
        graph::link_memories(db, &id, old_id, RelationType::Updates).await?;
    }

    // Store precomputed vector + spawn auto-linking (no double-embed)
    if let Some(vector) = precomputed_vector {
        // Store the vector on the memory record
        if let Err(e) = crate::db::embeddings::store_vector_pub(db, id.as_str(), vector.clone()).await {
            tracing::warn!(id = %id, "failed to store embedding: {e}");
        }

        // Spawn auto-linking with precomputed vector (no re-embedding).
        // Move vector — last use of it in this scope.
        auto_link::spawn_auto_link_with_vector(
            db.clone(),
            id.clone(),
            project_clone,
            summary_text.clone(),
            vector,
            tasks.clone(),
        ).await;
    }

    // If content is long, chunk it for fine-grained vector search.
    // Requires an embedder — skipped when called from execute_with_vector
    // (bench ingest, where content is typically short anyway).
    if let (Some(ref client), Some(parent)) = (embedding_client, chunk_parent) {
        let chunk_db = db.clone();
        let chunk_embedder = client.clone();
        let chunk_parent_id = id.clone();
        tasks.spawn(async move {
            if let Err(e) = chunking::chunk_and_embed(
                &chunk_db,
                chunk_embedder,
                &chunk_parent_id,
                &parent,
            )
            .await
            {
                tracing::warn!(id = %chunk_parent_id, "chunking failed: {e}");
            }
        }).await;
    }

    // Fire-and-forget: extract typed relations + anchor entities via LLM sidecar.
    // Runs for every visibility (owner decision 2026-10-24); skipped only when the sidecar is absent.
    // Summary-only — never pass full content as a prompt-injection defense baseline.
    //
    // Both anchor and relation extraction run concurrently via tokio::join!.
    // The aggregate result determines extraction_status on the memory record:
    //   both ok      → complete
    //   anchor err   → anchor_failed
    //   relation err → relation_failed
    //   both err     → both_failed
    // extraction_status is initialised to 'pending' at create time (in the CREATE
    // CONTENT block) so recall always sees a defined value even before the async
    // task runs.
    if let Some(sidecar) = llm_sidecar.as_ref() {
        let status_db = db.clone();
        let extract_db = db.clone();
        let extract_sidecar = sidecar.clone();
        let extract_id = id.clone();
        let extract_summary = summary_text.clone();
        let extract_project = project_for_extract;
        let extract_domain = domain.first().cloned();
        let extract_threshold = sidecar.confidence_threshold();

        let anchor_db = db.clone();
        let anchor_sidecar = sidecar.clone();
        let anchor_id = id.clone();
        let anchor_summary = summary_text.clone();
        let anchor_proj = project_for_anchors;
        let anchor_domain = domain.first().cloned();

        let status_id = id.clone();
        tasks.spawn(async move {
            let relation_fut = crate::llm::extract_and_persist(
                &extract_db,
                &extract_sidecar,
                &extract_id,
                &extract_summary,
                None, // summary-only — never send content to LLM (injection defense)
                extract_project.as_deref(),
                extract_domain.as_deref(),
                extract_threshold,
            );
            let anchor_fut = crate::llm::extractor::extract_anchors_and_persist(
                &anchor_db,
                &anchor_sidecar,
                &anchor_id,
                &anchor_summary,
                anchor_proj.as_deref(),
                anchor_domain.as_deref(),
            );

            let (relation_result, anchor_result) = tokio::join!(relation_fut, anchor_fut);

            let relation_ok = match relation_result {
                Ok(stats) => {
                    if stats.persisted > 0 {
                        tracing::info!(
                            id = %extract_id,
                            persisted = stats.persisted,
                            returned = stats.returned,
                            "typed_relation extraction"
                        );
                    }
                    true
                }
                Err(e) => {
                    tracing::warn!(id = %extract_id, "extract_and_persist failed: {e}");
                    false
                }
            };

            let anchor_ok = match anchor_result {
                Ok(count) if count > 0 => {
                    tracing::info!(id = %anchor_id, count, "anchor entities stored");
                    true
                }
                Ok(_) => true, // 0 anchors is not an error
                Err(e) => {
                    tracing::warn!(id = %anchor_id, "extract_anchors_and_persist failed: {e}");
                    false
                }
            };

            let agg_status = match (anchor_ok, relation_ok) {
                (true, true) => crate::types::ExtractionStatus::Complete,
                (false, true) => crate::types::ExtractionStatus::AnchorFailed,
                (true, false) => crate::types::ExtractionStatus::RelationFailed,
                (false, false) => crate::types::ExtractionStatus::BothFailed,
            };

            if let Err(e) = crate::db::memory::set_extraction_status(&status_db, &status_id, agg_status).await {
                tracing::warn!(id = %status_id, "set_extraction_status failed: {e}");
            }
        }).await;
    }

    // Fire-and-forget: refresh agent's last_active timestamp.
    if let Some(ref aid) = resolved_author {
        let touch_db = db.clone();
        let touch_id = aid.clone();
        tasks.spawn(async move {
            if let Err(e) = agent::touch_last_active(&touch_db, &touch_id).await {
                tracing::warn!(agent_id = %touch_id, "touch_last_active failed: {e}");
            }
        }).await;
    }

    let message = match warning_msg {
        Some(warn) => format!("Memory stored (warning: {warn})"),
        None => "Memory stored".to_string(),
    };

    Ok(RememberOutput {
        id: id.to_string(),
        message,
    })
}
