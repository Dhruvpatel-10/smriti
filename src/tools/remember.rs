use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{auto_link, chunking, client::SurrealClient, dedup, embeddings::LocalEmbedder, graph, memory, validation};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberInput {
    /// 50-100 token summary of the memory (REQUIRED)
    pub summary: String,
    /// Memory type category (REQUIRED)
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    /// Memory subtype — must be valid for the given type (REQUIRED)
    pub subtype: MemorySubtype,
    /// Source of this memory
    pub source: Source,
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
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RememberOutput {
    pub id: String,
    pub message: String,
}

#[tracing::instrument(skip_all, fields(tool = "kb_remember"))]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<LocalEmbedder>>,
    input: RememberInput,
) -> Result<RememberOutput> {
    // ── Phase 1: Validate (synchronous, zero latency) ──────────────────

    validation::validate_summary(&input.summary)?;
    validation::validate_content(input.content.as_deref())?;

    let memory_type = input.memory_type;
    let subtype = input.subtype;
    if !subtype.valid_for(memory_type) {
        return Err(crate::error::KbError::InvalidInput(format!(
            "subtype {subtype} is not valid for type {memory_type}"
        )));
    }

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
    let visibility = input.visibility.unwrap_or_default();

    // ── Phase 2: Embed + Dedup (BEFORE create) ─────────────────────────
    // Contextual embedding: prepend metadata for better similarity
    let embed_text = validation::contextual_text(
        &input.summary,
        memory_type,
        subtype,
        input.project.as_deref(),
        &domain,
    );

    let mut warning_msg: Option<String> = None;
    let mut precomputed_vector: Option<Vec<f32>> = None;

    if let Some(ref client) = embedding_client {
        match client.embed(&embed_text).await {
            Ok(vector) => {
                // Dedup check BEFORE creating the memory — no orphaned records
                match dedup::check_duplicate(db, &vector).await {
                    Ok(dedup::DedupResult::Duplicate { existing_id, similarity }) => {
                        tracing::info!(existing_id = %existing_id, similarity, "dedup rejected duplicate");
                        return Err(crate::error::KbError::InvalidInput(format!(
                            "duplicate memory detected (similarity {similarity:.2}): existing memory:{existing_id}"
                        )));
                    }
                    Ok(dedup::DedupResult::Warning { similar_id, similarity }) => {
                        warning_msg = Some(format!(
                            "near-duplicate (similarity {similarity:.2}) of memory:{similar_id}"
                        ));
                    }
                    Ok(dedup::DedupResult::Clean) => {}
                    Err(e) => {
                        tracing::warn!("dedup check failed (storing anyway): {e}");
                    }
                }
                precomputed_vector = Some(vector);
            }
            Err(e) => {
                tracing::warn!("embedding failed (storing without vector): {e}");
            }
        }
    }

    // ── Phase 3: Create memory ─────────────────────────────────────────

    let summary_text = input.summary.clone();
    let project_clone = input.project.clone();
    let has_long_content = input
        .content
        .as_ref()
        .is_some_and(|c| c.len() >= chunking::CHUNK_THRESHOLD);

    let id = memory::create(
        db,
        input.summary,
        input.content,
        memory_type,
        subtype,
        input.source,
        input.project,
        domain.clone(),
        people.clone(),
        importance,
        visibility,
        input.event_at,
        tags,
        enforced_ttl,
        input.source_agent,
        None,
    )
    .await?;

    // ── Phase 4: Post-create (graph edges, vector store, auto-link) ────

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

    // Handle supersession — atomic conditional update prevents race conditions.
    // Two concurrent kb_remember calls targeting the same updates_id: only the
    // winner (whose conditional UPDATE matched status='active') creates the edge.
    if let Some(updates_id) = input.updates_id {
        let old_id = MemoryId::from(updates_id);
        match memory::supersede(db, &old_id).await {
            Ok(true) => {
                tracing::info!(new_id = %id, old_id = %old_id, "memory superseded");
                // Supersession succeeded — create the updates edge.
                graph::link_memories(db, &id, &old_id, RelationType::Updates).await?;
            }
            Ok(false) => {
                // Already superseded/archived by another call, or doesn't exist.
                tracing::warn!(old_id = %old_id, "supersession target not active or not found, skipping edge");
            }
            Err(e) => {
                tracing::warn!(old_id = %old_id, "supersession failed: {e}");
            }
        }
    }

    // Store precomputed vector + spawn auto-linking (no double-embed)
    if let Some(ref client) = embedding_client {
        if let Some(ref vector) = precomputed_vector {
            // Store the vector on the memory record
            if let Err(e) = crate::db::embeddings::store_vector_pub(db, id.as_str(), vector).await {
                tracing::warn!(id = %id, "failed to store embedding: {e}");
            }

            // Spawn auto-linking with precomputed vector (no re-embedding)
            auto_link::spawn_auto_link_with_vector(
                db.clone(),
                id.clone(),
                project_clone,
                summary_text.clone(),
                vector.clone(),
            );
        }

        // If content is long, chunk it for fine-grained vector search
        if has_long_content {
            let chunk_db = db.clone();
            let chunk_embedder = client.clone();
            let chunk_parent_id = id.clone();
            tokio::spawn(async move {
                match memory::get(&chunk_db, &chunk_parent_id).await {
                    Ok(parent) => {
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
                    }
                    Err(e) => {
                        tracing::warn!(id = %chunk_parent_id, "failed to fetch for chunking: {e}");
                    }
                }
            });
        }
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
