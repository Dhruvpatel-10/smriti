use chrono::Utc;
use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::dedup::{DedupResult, REJECT_THRESHOLD, WARN_THRESHOLD};
use crate::db::helpers::{enum_to_value, record_id_bare};
use crate::db::validation;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::*;

/// All inputs to `memory::create`. Required fields (summary/type/subtype)
/// are constructor params; optional fields default to empty/None and are set
/// via builder-style methods so call sites don't have to thread eighteen
/// positional arguments.
#[derive(Debug, Clone)]
pub struct MemoryDraft {
    pub summary: String,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub channel: Option<Channel>,
    pub modality: Option<Modality>,
    pub content: Option<String>,
    pub project: Option<String>,
    pub domain: Vec<String>,
    pub people: Vec<String>,
    pub importance: Importance,
    pub visibility: Visibility,
    pub event_at: Option<chrono::DateTime<chrono::Utc>>,
    pub tags: Vec<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub source_agent: Option<String>,
    pub session_id: Option<String>,
    pub author: Option<AgentId>,
    pub valid_until: Option<chrono::DateTime<chrono::Utc>>,
    /// When `None`, `memory::create` derives the value via
    /// `validation::classify_static`. Set explicitly to override.
    pub is_static: Option<bool>,
}

impl MemoryDraft {
    pub fn new(
        summary: String,
        memory_type: MemoryType,
        subtype: MemorySubtype,
        channel: Option<Channel>,
        modality: Option<Modality>,
    ) -> Self {
        Self {
            summary,
            memory_type,
            subtype,
            channel,
            modality,
            content: None,
            project: None,
            domain: vec![],
            people: vec![],
            importance: Importance::default(),
            visibility: Visibility::default(),
            event_at: None,
            tags: vec![],
            expires_at: None,
            source_agent: None,
            session_id: None,
            author: None,
            valid_until: None,
            is_static: None,
        }
    }

    pub fn content(mut self, c: String) -> Self {
        self.content = Some(c);
        self
    }
    pub fn maybe_content(mut self, c: Option<String>) -> Self {
        self.content = c;
        self
    }
    pub fn project(mut self, p: String) -> Self {
        self.project = Some(p);
        self
    }
    pub fn maybe_project(mut self, p: Option<String>) -> Self {
        self.project = p;
        self
    }
    pub fn domain(mut self, d: Vec<String>) -> Self {
        self.domain = d;
        self
    }
    pub fn people(mut self, p: Vec<String>) -> Self {
        self.people = p;
        self
    }
    pub fn importance(mut self, i: Importance) -> Self {
        self.importance = i;
        self
    }
    pub fn visibility(mut self, v: Visibility) -> Self {
        self.visibility = v;
        self
    }
    pub fn event_at(mut self, ts: chrono::DateTime<chrono::Utc>) -> Self {
        self.event_at = Some(ts);
        self
    }
    pub fn maybe_event_at(mut self, ts: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        self.event_at = ts;
        self
    }
    pub fn tags(mut self, t: Vec<String>) -> Self {
        self.tags = t;
        self
    }
    pub fn expires_at(mut self, ts: chrono::DateTime<chrono::Utc>) -> Self {
        self.expires_at = Some(ts);
        self
    }
    pub fn maybe_expires_at(mut self, ts: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        self.expires_at = ts;
        self
    }
    pub fn source_agent(mut self, name: String) -> Self {
        self.source_agent = Some(name);
        self
    }
    pub fn maybe_source_agent(mut self, name: Option<String>) -> Self {
        self.source_agent = name;
        self
    }
    pub fn session_id(mut self, id: String) -> Self {
        self.session_id = Some(id);
        self
    }
    pub fn maybe_session_id(mut self, id: Option<String>) -> Self {
        self.session_id = id;
        self
    }
    pub fn author(mut self, a: AgentId) -> Self {
        self.author = Some(a);
        self
    }
    pub fn maybe_author(mut self, a: Option<AgentId>) -> Self {
        self.author = a;
        self
    }
    pub fn valid_until(mut self, ts: chrono::DateTime<chrono::Utc>) -> Self {
        self.valid_until = Some(ts);
        self
    }
    pub fn maybe_valid_until(mut self, ts: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        self.valid_until = ts;
        self
    }
    pub fn is_static(mut self, b: bool) -> Self {
        self.is_static = Some(b);
        self
    }
    pub fn maybe_is_static(mut self, b: Option<bool>) -> Self {
        self.is_static = b;
        self
    }
}

/// Create a new memory record.
#[instrument(skip(db, draft), fields(memory_type = %draft.memory_type, subtype = %draft.subtype))]
pub async fn create(db: &SurrealClient, draft: MemoryDraft) -> Result<MemoryId> {
    if !draft.subtype.valid_for(draft.memory_type) {
        return Err(KbError::InvalidInput(format!(
            "subtype {} is not valid for type {}",
            draft.subtype, draft.memory_type
        )));
    }

    let id = MemoryId::new();
    let now = Utc::now();
    let event = draft.event_at.unwrap_or(now);

    // Derive is_static from subtype/importance unless caller overrode it.
    let is_static = draft
        .is_static
        .unwrap_or_else(|| validation::classify_static(draft.subtype, draft.importance));

    // Calculate TTL-based expiry if not explicitly set. Static memories never
    // get a TTL — facts you've explicitly marked permanent shouldn't expire.
    let expires = if is_static {
        draft.expires_at
    } else {
        draft
            .expires_at
            .or_else(|| validation::ttl_duration(draft.memory_type, draft.subtype).map(|d| now + d))
    };

    // author is stored as option<record<agent>>; None → NONE (SurrealDB null).
    let author_rid: Option<RecordId> = draft
        .author
        .as_ref()
        .map(|aid| RecordId::new("agent", aid.as_str()));

    db.query(
        "CREATE type::record($table, $id) CONTENT {
            summary: $summary,
            content: $content,
            memory_type: $memory_type,
            subtype: $subtype,
            channel: $channel,
            modality: $modality,
            source_ref: NONE,
            project: $project,
            domain: $domain,
            people: $people,
            importance: $importance,
            visibility: $visibility,
            status: 'active',
            tags: $tags,
            access_count: 0,
            event_at: $event_at,
            created_at: $created_at,
            updated_at: $updated_at,
            expires_at: $expires_at,
            source_agent: $source_agent,
            session_id: $session_id,
            author: $author,
            valid_until: $valid_until,
            invalidated_at: NONE,
            invalidation_reason: NONE,
            is_static: $is_static,
            extraction_status: $extraction_status
        }",
    )
    .bind(("table", "memory"))
    .bind(("id", id.as_str().to_string()))
    .bind(("extraction_status", crate::types::ExtractionStatus::Pending.as_str().to_string()))
    .bind(("summary", draft.summary))
    .bind(("content", draft.content))
    .bind(("memory_type", enum_to_value(draft.memory_type)?))
    .bind(("subtype", enum_to_value(draft.subtype)?))
    .bind(("channel", draft.channel.map(|c| c.to_string())))
    .bind(("modality", draft.modality.map(|m| m.to_string())))
    .bind(("project", draft.project))
    .bind(("domain", draft.domain))
    .bind(("people", draft.people))
    .bind(("importance", draft.importance.value() as i64))
    .bind(("visibility", enum_to_value(draft.visibility)?))
    .bind(("tags", draft.tags))
    .bind(("event_at", event))
    .bind(("created_at", now))
    .bind(("updated_at", now))
    .bind(("expires_at", expires))
    .bind(("source_agent", draft.source_agent))
    .bind(("session_id", draft.session_id))
    .bind(("author", author_rid))
    .bind(("valid_until", draft.valid_until))
    .bind(("is_static", is_static))
    .await
    .map_err(|e| KbError::db(format!("creating memory {}", &id), e))?;

    Ok(id)
}

/// Prefix embedded in THROW messages so we can detect dedup rejections vs
/// genuine DB errors. Must not contain characters that break SurrealQL strings.
const DEDUP_REJECT_PREFIX: &str = "SMRITI_DEDUP_REJECT:";

/// Atomically run dedup check + CREATE memory in a single SurrealDB transaction.
///
/// The transaction structure:
///   0. BEGIN TRANSACTION
///   1. LET $dup = (SELECT cosine scan scoped to author bucket)
///   2. IF score > REJECT_THRESHOLD THEN THROW "SMRITI_DEDUP_REJECT:<id>" END
///   3. CREATE type::record('memory', $id) CONTENT {...}
///   4. COMMIT TRANSACTION
///
/// Because SurrealKV is a single-writer store, the BEGIN…COMMIT block
/// serialises concurrent writers — no two transactions overlap. A THROW in
/// step 2 rolls back the transaction before step 3 executes, so no phantom
/// record is created on rejection.
///
/// Returns `(MemoryId, Option<DedupResult::Warning>)`. Warning carries the
/// near-duplicate ID so the caller can surface it in the response message.
/// `KbError::InvalidInput` is returned when THROW fires (duplicate detected).
#[instrument(skip(db, draft, embedding), fields(memory_type = %draft.memory_type, subtype = %draft.subtype))]
pub async fn create_with_dedup_atomic(
    db: &SurrealClient,
    draft: MemoryDraft,
    embedding: Vec<f32>,
    author: Option<&AgentId>,
) -> Result<(MemoryId, Option<DedupResult>)> {
    if !draft.subtype.valid_for(draft.memory_type) {
        return Err(KbError::InvalidInput(format!(
            "subtype {} is not valid for type {}",
            draft.subtype, draft.memory_type
        )));
    }

    let id = MemoryId::new();
    let now = Utc::now();
    let event = draft.event_at.unwrap_or(now);

    let is_static = draft
        .is_static
        .unwrap_or_else(|| validation::classify_static(draft.subtype, draft.importance));

    let expires = if is_static {
        draft.expires_at
    } else {
        draft
            .expires_at
            .or_else(|| validation::ttl_duration(draft.memory_type, draft.subtype).map(|d| now + d))
    };

    let author_rid: Option<RecordId> = draft
        .author
        .as_ref()
        .map(|aid| RecordId::new("agent", aid.as_str()));

    // Build the author-scoped dedup SELECT. Two branches to avoid binding NONE
    // as an author_id (SurrealDB serialises Option::None differently per context).
    let (dedup_sql, needs_author_bind) = if author.is_some() {
        (
            format!(
                "LET $dup = (SELECT record::id(id) AS mem_id, \
                    vector::similarity::cosine(vector, $qv) AS score \
                    FROM memory \
                    WHERE {BITEMPORAL_ACTIVE} AND vector != NONE AND author = $author_id \
                    ORDER BY score DESC LIMIT 1);\n"
            ),
            true,
        )
    } else {
        (
            format!(
                "LET $dup = (SELECT record::id(id) AS mem_id, \
                    vector::similarity::cosine(vector, $qv) AS score \
                    FROM memory \
                    WHERE {BITEMPORAL_ACTIVE} AND vector != NONE AND author IS NONE \
                    ORDER BY score DESC LIMIT 1);\n"
            ),
            false,
        )
    };

    let sql = format!(
        "BEGIN TRANSACTION;\n\
         {dedup_sql}\
         IF array::len($dup) > 0 AND $dup[0].score > {REJECT_THRESHOLD} THEN \
           THROW string::concat('{DEDUP_REJECT_PREFIX}', $dup[0].mem_id) \
         END;\n\
         CREATE type::record($table, $id) CONTENT {{\
            summary: $summary, \
            content: $content, \
            memory_type: $memory_type, \
            subtype: $subtype, \
            channel: $channel, \
            modality: $modality, \
            source_ref: NONE, \
            project: $project, \
            domain: $domain, \
            people: $people, \
            importance: $importance, \
            visibility: $visibility, \
            status: 'active', \
            tags: $tags, \
            access_count: 0, \
            event_at: $event_at, \
            created_at: $created_at, \
            updated_at: $updated_at, \
            expires_at: $expires_at, \
            source_agent: $source_agent, \
            session_id: $session_id, \
            author: $author, \
            valid_until: $valid_until, \
            invalidated_at: NONE, \
            invalidation_reason: NONE, \
            is_static: $is_static, \
            extraction_status: $extraction_status \
         }};\n\
         COMMIT TRANSACTION;\n"
    );

    let mut q = db
        .query(&sql)
        .bind(("qv", embedding))
        .bind(("table", "memory"))
        .bind(("id", id.as_str().to_string()))
        .bind(("extraction_status", crate::types::ExtractionStatus::Pending.as_str().to_string()))
        .bind(("summary", draft.summary))
        .bind(("content", draft.content))
        .bind(("memory_type", enum_to_value(draft.memory_type)?))
        .bind(("subtype", enum_to_value(draft.subtype)?))
        .bind(("channel", draft.channel.map(|c| c.to_string())))
        .bind(("modality", draft.modality.map(|m| m.to_string())))
        .bind(("project", draft.project))
        .bind(("domain", draft.domain))
        .bind(("people", draft.people))
        .bind(("importance", draft.importance.value() as i64))
        .bind(("visibility", enum_to_value(draft.visibility)?))
        .bind(("tags", draft.tags))
        .bind(("event_at", event))
        .bind(("created_at", now))
        .bind(("updated_at", now))
        .bind(("expires_at", expires))
        .bind(("source_agent", draft.source_agent))
        .bind(("session_id", draft.session_id))
        .bind(("author", author_rid))
        .bind(("valid_until", draft.valid_until))
        .bind(("is_static", is_static));

    if needs_author_bind {
        let agent_rid = RecordId::new("agent", author.unwrap().as_str());
        q = q.bind(("author_id", agent_rid));
    }

    let mut resp = q
        .await
        .map_err(|e| KbError::db(format!("atomic dedup+create for memory {id}"), e))?;

    // Statement layout: BEGIN(0), LET $dup(1), IF/THROW/END(2), CREATE(3), COMMIT(4).
    //
    // When THROW fires at index 2, the outer `.await` still returns Ok(resp) —
    // the THROW is embedded in the per-statement response, not the top-level result.
    // We detect it by inspecting the take(2) error for the DEDUP_REJECT_PREFIX.
    // If THROW fired, index 3 (CREATE) and 4 (COMMIT) were skipped and will also
    // return "query not executed due to a cancelled transaction" errors, which we
    // ignore once we know it was a dedup rejection.

    // LET result at index 1 (defensive: unwrap_or_default if LET doesn't surface a value).
    let dup_rows: Vec<serde_json::Value> = resp.take(1).unwrap_or_default();

    // Index 2: IF/THROW/END result — error here means THROW fired.
    let throw_result: std::result::Result<Vec<serde_json::Value>, _> = resp.take(2);
    if let Err(ref e) = throw_result {
        let msg = e.to_string();
        if msg.contains(DEDUP_REJECT_PREFIX) {
            // Extract the existing_id embedded after the prefix.
            let after = msg
                .find(DEDUP_REJECT_PREFIX)
                .map(|pos| &msg[pos + DEDUP_REJECT_PREFIX.len()..])
                .unwrap_or("");
            // Strip trailing quote/whitespace SurrealDB may append.
            let existing_id = after
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .to_string();
            tracing::info!(existing_id = %existing_id, "atomic dedup rejected duplicate");
            return Err(KbError::InvalidInput(format!(
                "duplicate memory detected: existing memory:{existing_id}"
            )));
        }
        // Other IF-statement error — not a dedup THROW, propagate as db error.
        return Err(KbError::Database {
            context: "atomic dedup+create IF statement".to_string(),
            source: Box::new(e.clone()),
        });
    }

    // Verify CREATE succeeded at index 3.
    let _created: Vec<serde_json::Value> = resp
        .take(3)
        .map_err(|e| KbError::db("reading create result from atomic txn", e))?;

    // Build warning if the near-duplicate band was hit.
    let warning = dup_rows.first().and_then(|row| {
        let score = row.get("score").and_then(|v| v.as_f64())?;
        let mem_id = row.get("mem_id").and_then(|v| v.as_str())?.to_string();
        if score > WARN_THRESHOLD && score <= REJECT_THRESHOLD && !mem_id.is_empty() {
            Some(DedupResult::Warning { similar_id: mem_id, similarity: score })
        } else {
            None
        }
    });

    Ok((id, warning))
}

/// Get a single memory by ID.
#[instrument(skip(db))]
pub async fn get(db: &SurrealClient, id: &MemoryId) -> Result<Memory> {
    let mut result = db
        .query("SELECT * FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("getting memory {id}"), e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing memory {id}"), e))?;

    let row = row.ok_or_else(|| KbError::NotFound(id.to_string()))?;
    parse_memory_row(row, id)
}

/// Fetch a memory by ID with visibility enforcement and access_count increment
/// for the kb_drill path.
///
/// Sensitive memories are blocked unless the caller supplies the project they
/// belong to — closes the cross-project information leak via direct ID fetch.
/// Returns `None` (not `NotFound`) when the record exists but is filtered out
/// so the caller can emit a generic not-found response without leaking existence.
/// Active-status filter matches `get_and_touch` behaviour.
#[instrument(skip(db))]
pub async fn get_for_drill(
    db: &SurrealClient,
    id: &MemoryId,
    project: Option<&str>,
) -> Result<Option<Memory>> {
    // Two explicit branches by design: the visibility predicate is a security
    // boundary, and a unified `$project IS NOT NONE OR …` predicate would
    // depend on how the SurrealDB driver serialises `Option::None` (NONE vs
    // JSON null). Memories with `project = NONE` would leak when the caller
    // also supplied no project. The two-branch form is invariant of driver
    // semantics — see devils-advocate audit, attack #1.
    let mut result = if let Some(p) = project {
        db.query(format!(
            "UPDATE type::record('memory', $id)
             SET access_count += 1, updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE} AND (visibility != 'sensitive' OR project = $project)
             RETURN AFTER"
        ))
        .bind(("id", id.as_str().to_string()))
        .bind(("project", p.to_string()))
        .await
        .map_err(|e| KbError::db(format!("drill-fetch memory {id}"), e))?
    } else {
        db.query(format!(
            "UPDATE type::record('memory', $id)
             SET access_count += 1, updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE} AND visibility != 'sensitive'
             RETURN AFTER"
        ))
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("drill-fetch memory {id}"), e))?
    };

    let updated: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing drill-fetch result for {id}"), e))?;

    match updated.into_iter().next() {
        None => Ok(None),
        Some(r) => parse_memory_row(r, id).map(Some),
    }
}

/// Get full memory and increment access_count.
///
/// Returns `NotFound` if the memory does not exist or is not active — callers must
/// not silently ignore a missing/archived target.
#[instrument(skip(db))]
pub async fn get_and_touch(db: &SurrealClient, id: &MemoryId) -> Result<Memory> {
    let mut result = db
        .query(format!(
            "UPDATE type::record('memory', $id)
             SET access_count += 1, updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE}
             RETURN AFTER"
        ))
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("touching memory {id}"), e))?;

    let updated: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing touch result for {id}"), e))?;

    let row = updated
        .into_iter()
        .next()
        .ok_or_else(|| KbError::NotFound(id.to_string()))?;
    parse_memory_row(row, id)
}

/// Mark a memory as archived (soft delete), recording why it was invalidated.
///
/// Idempotent: if `invalidated_at` is already set, the earlier timestamp and reason
/// are preserved — this call is a no-op for already-invalidated memories.
/// Returns `NotFound` only if the record does not exist at all.
#[instrument(skip(db), fields(reason = %reason))]
pub async fn archive(db: &SurrealClient, id: &MemoryId, reason: InvalidationReason) -> Result<()> {
    // Conditional UPDATE preserves prior invalidation; RETURN BEFORE = Some
    // means we matched (existed and was active). None = either missing or
    // already invalidated — cheap second SELECT disambiguates.
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id)
             SET status = 'archived',
                 invalidated_at = time::now(),
                 invalidation_reason = $reason,
                 valid_until = IF valid_until IS NONE THEN time::now() ELSE valid_until END,
                 updated_at = time::now()
             WHERE invalidated_at IS NONE
             RETURN BEFORE",
        )
        .bind(("id", id.as_str().to_string()))
        .bind(("reason", reason.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("archiving memory {id}"), e))?;

    let before: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing archive result for {id}"), e))?;

    if before.is_some() {
        return Ok(());
    }

    // Either record missing or already-archived — only NotFound is an error.
    let mut exists_result = db
        .query("SELECT id FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("checking memory {id} exists after archive miss"), e))?;
    let exists: Option<serde_json::Value> = exists_result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing exists check for {id}"), e))?;
    if exists.is_none() {
        return Err(KbError::NotFound(id.to_string()));
    }
    // Already invalidated — idempotent no-op.
    Ok(())
}

/// Atomically mark a memory as superseded, only if it is currently active.
///
/// Sets `valid_until` to the superseding memory's `event_at` (when the new fact
/// became true), `invalidated_at` to now, `invalidation_reason` to 'superseded',
/// and `status` to 'archived'. `status='archived'` is the invariant; the reason
/// field is the source of truth for *why* it was archived.
///
/// Returns `true` if the supersession succeeded (status was active),
/// `false` if the memory was already superseded/archived by a concurrent call.
#[instrument(skip(db))]
pub async fn supersede(
    db: &SurrealClient,
    id: &MemoryId,
    superseder_event_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id)
             SET status = 'archived',
                 valid_until = $event_at,
                 invalidated_at = time::now(),
                 invalidation_reason = 'superseded',
                 updated_at = time::now()
             WHERE invalidated_at IS NONE
             RETURN BEFORE"
        )
        .bind(("id", id.as_str().to_string()))
        .bind(("event_at", superseder_event_at))
        .await
        .map_err(|e| KbError::db(format!("superseding memory {id}"), e))?;

    let before: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("checking supersede result for {id}"), e))?;

    // If RETURN BEFORE is None, the conditional UPDATE matched no rows —
    // either the ID doesn't exist or status was already non-active.
    Ok(before.is_some())
}

/// Restore a superseded memory to active status — used only as a rollback when the
/// new-memory creation fails after a successful supersede. Guarded by
/// `WHERE invalidation_reason = 'superseded'` to ensure we only undo a supersession,
/// not some other archival. Also clears the bitemporal invalidation fields.
///
/// Returns `true` if the rollback matched,
/// `false` if someone else already changed the status (treat as best-effort).
#[instrument(skip(db))]
pub async fn unsupersede(db: &SurrealClient, id: &MemoryId) -> Result<bool> {
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id)
             SET status = 'active',
                 valid_until = NONE,
                 invalidated_at = NONE,
                 invalidation_reason = NONE,
                 updated_at = time::now()
             WHERE invalidation_reason = 'superseded'
             RETURN BEFORE"
        )
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("unsuperseding memory {id}"), e))?;

    let before: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("checking unsupersede result for {id}"), e))?;

    Ok(before.is_some())
}

/// Update the extraction_status field on a memory record.
///
/// Only transitions toward success: if the current status is already `complete`
/// or `skipped`, this call is a no-op to avoid overwriting a prior success with a
/// retry-attempt failure from a race.
///
/// Retry paths (failed → complete) always call this unconditionally because the
/// guard above prevents over-writing a successful status.
#[instrument(skip(db))]
pub async fn set_extraction_status(
    db: &SurrealClient,
    id: &MemoryId,
    status: crate::types::ExtractionStatus,
) -> Result<()> {
    let id_rid = surrealdb::types::RecordId::new("memory", id.as_str());
    db.query(
        "UPDATE $id SET extraction_status = $status, updated_at = time::now() \
         WHERE extraction_status != 'complete' AND extraction_status != 'skipped'",
    )
    .bind(("id", id_rid))
    .bind(("status", status.as_str().to_string()))
    .await
    .map_err(|e| KbError::db(format!("setting extraction_status on memory {id}"), e))?;
    Ok(())
}

/// Persist anchor entities extracted by the LLM sidecar onto a memory record.
///
/// No-op-safe: calling with an empty slice skips the write so a future
/// populated value is never overwritten with null.
#[instrument(skip(db, anchors))]
pub async fn set_anchors(db: &SurrealClient, id: &MemoryId, anchors: &[String]) -> Result<()> {
    if anchors.is_empty() {
        return Ok(());
    }
    let id_rid = RecordId::new("memory", id.as_str());
    db.query("UPDATE $id SET anchor_entities = $anchors, updated_at = time::now()")
        .bind(("id", id_rid))
        .bind(("anchors", anchors.to_vec()))
        .await
        .map_err(|e| KbError::db(format!("setting anchors on memory {id}"), e))?;
    Ok(())
}

/// Promote a memory: clear expiry, bump importance to at least 3.
///
/// Returns `NotFound` if the memory does not exist or is not active.
///
/// Single-RTT: `math::max([importance, 3])` clamps in-DB so the client does
/// not need to read-then-write. Closes the read-then-write TOCTOU window where
/// a concurrent decay could change `importance` between the read and the
/// UPDATE. `RETURN NONE` skips serialising the full row back.
#[instrument(skip(db))]
pub async fn promote(db: &SurrealClient, id: &MemoryId) -> Result<()> {
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id) SET
                expires_at = NONE,
                importance = math::max([importance, 3]),
                updated_at = time::now()
             WHERE invalidated_at IS NONE
             RETURN BEFORE",
        )
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("promoting memory {id}"), e))?;

    let updated: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing promote result for {id}"), e))?;

    if updated.is_empty() {
        // Disambiguate missing-vs-already-invalidated for a clearer error.
        let mut exists_result = db
            .query("SELECT id FROM type::record('memory', $id)")
            .bind(("id", id.as_str().to_string()))
            .await
            .map_err(|e| KbError::db(format!("checking memory {id} exists after promote miss"), e))?;
        let exists: Option<serde_json::Value> = exists_result
            .take(0)
            .map_err(|e| KbError::db(format!("deserializing exists check for {id}"), e))?;
        return if exists.is_some() {
            Err(KbError::InvalidInput(format!(
                "memory {id} is invalidated and cannot be promoted"
            )))
        } else {
            Err(KbError::NotFound(id.to_string()))
        };
    }

    Ok(())
}

/// Retract a memory: explicit "I was wrong" invalidation.
///
/// Distinct from `archive`/`kb_forget` which marks "no longer relevant".
/// Uses `InvalidationReason::Retracted`.
///
/// Implementation: the conditional UPDATE (`WHERE invalidated_at IS NONE`)
/// atomically claims the retraction — at most one concurrent caller wins.
/// The optional RELATE runs only when the UPDATE matched a row, so an
/// already-invalidated target cannot produce a spurious `retracts` edge.
///
/// Atomicity tradeoff: UPDATE and RELATE are not wrapped in a single
/// transaction. If the network drops between them the retraction commits
/// without the audit edge — a degraded audit trail but no data corruption.
/// The `idx_retracts_uniq` index makes RELATE idempotent.
///
/// Idempotency: a second retract on an already-invalidated target returns
/// `InvalidInput("already invalidated")` — we preserve the first invalidation
/// rather than silently overwriting a prior reason. Disambiguates missing-vs-
/// already-invalidated by a follow-up SELECT only when the UPDATE matched zero.
#[instrument(skip(db), fields(has_correction = correction_id.is_some()))]
pub async fn retract(
    db: &SurrealClient,
    id: &MemoryId,
    correction_id: Option<&MemoryId>,
) -> Result<()> {
    // Validate correction exists before attempting the UPDATE so a typo'd
    // correction_id surfaces as NotFound rather than orphaning a retraction
    // without its audit edge.
    if let Some(corr_id) = correction_id {
        let mut corr_result = db
            .query("SELECT id FROM type::record('memory', $cid)")
            .bind(("cid", corr_id.as_str().to_string()))
            .await
            .map_err(|e| KbError::db(format!("checking correction memory {corr_id} exists"), e))?;

        let corr_exists: Option<serde_json::Value> = corr_result
            .take(0)
            .map_err(|e| KbError::db(format!("deserializing correction check for {corr_id}"), e))?;

        if corr_exists.is_none() {
            return Err(KbError::NotFound(format!(
                "correction memory {corr_id} not found"
            )));
        }
    }

    // Conditional UPDATE — only the first concurrent retract wins.
    let mut update_result = db
        .query(
            "UPDATE type::record('memory', $id) \
               SET invalidated_at = time::now(), \
                   invalidation_reason = $reason, \
                   status = 'archived', \
                   valid_until = IF valid_until IS NONE THEN time::now() ELSE valid_until END, \
                   updated_at = time::now() \
               WHERE invalidated_at IS NONE \
               RETURN BEFORE",
        )
        .bind(("id", id.as_str().to_string()))
        .bind(("reason", InvalidationReason::Retracted.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("retracting memory {id}"), e))?;

    let updated: Vec<serde_json::Value> = update_result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing retract update for {id}"), e))?;

    if updated.is_empty() {
        // Disambiguate: does the row exist at all?
        let mut exists_result = db
            .query("SELECT id FROM type::record('memory', $id)")
            .bind(("id", id.as_str().to_string()))
            .await
            .map_err(|e| {
                KbError::db(format!("checking memory {id} exists after retract miss"), e)
            })?;
        let exists: Option<serde_json::Value> = exists_result
            .take(0)
            .map_err(|e| KbError::db(format!("deserializing exists check for {id}"), e))?;
        return if exists.is_some() {
            Err(KbError::InvalidInput(format!(
                "memory {id} is already invalidated"
            )))
        } else {
            Err(KbError::NotFound(id.to_string()))
        };
    }

    // UPDATE matched — only now create the audit edge. Idempotent under the
    // `idx_retracts_uniq` constraint if a retry races with itself.
    if let Some(corr_id) = correction_id {
        let target_rid = surrealdb::types::RecordId::new("memory", id.as_str());
        let corr_rid = surrealdb::types::RecordId::new("memory", corr_id.as_str());
        db.query("RELATE $corr_rid -> retracts -> $target_rid")
            .bind(("target_rid", target_rid))
            .bind(("corr_rid", corr_rid))
            .await
            .map_err(|e| {
                KbError::db(
                    format!("creating retracts edge from {corr_id} to {id}"),
                    e,
                )
            })?;
    }

    tracing::info!(memory_id = %id, "memory retracted");
    Ok(())
}

/// Count all memories, optionally filtered by status.
pub async fn count(db: &SurrealClient, status: Option<MemoryStatus>) -> Result<u64> {
    let (query, binds) = match status {
        Some(s) => {
            let sv = enum_to_value(s)?;
            ("SELECT count() AS total FROM memory WHERE status = $status GROUP ALL", Some(sv))
        }
        None => ("SELECT count() AS total FROM memory GROUP ALL", None),
    };

    let mut result = if let Some(sv) = binds {
        db.query(query).bind(("status", sv)).await
    } else {
        db.query(query).await
    }
    .map_err(|e| KbError::db("counting memories", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing count", e))?;

    Ok(row
        .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
        .unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn parse_memory_row(row: serde_json::Value, id: &MemoryId) -> Result<Memory> {
    // SurrealDB returns `id` as a RecordId object; we normalize to our MemoryId.
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => return Err(KbError::NotFound(id.to_string())),
    };

    // Normalize the id field to a plain string
    if let Some(id_val) = map.get("id") {
        let id_str = record_id_bare(id_val, "memory").unwrap_or_else(|| id.as_str().to_string());
        map.insert("id".to_string(), serde_json::Value::String(id_str));
    }

    // Wrap importance int into our newtype format
    if let Some(imp) = map.get("importance").and_then(|v| v.as_u64()) {
        map.insert("importance".to_string(), serde_json::json!(imp));
    }

    // Normalise `author`: stored as option<record<agent>>, SurrealDB may return
    // a RecordId object or "agent:<id>" string — strip to bare AgentId.
    // If the driver returns an unrecognised shape (record_id_bare → None) or
    // an empty string, treat as NULL rather than letting `AgentId("")` slip
    // through and silently misclassify the memory in author-filter queries.
    if let Some(author_val) = map.get("author").cloned()
        && !author_val.is_null()
    {
        match record_id_bare(&author_val, "agent").filter(|s| !s.is_empty()) {
            Some(bare) => {
                map.insert("author".to_string(), serde_json::Value::String(bare));
            }
            None => {
                map.insert("author".to_string(), serde_json::Value::Null);
            }
        }
    }

    serde_json::from_value(serde_json::Value::Object(map)).map_err(|e| {
        KbError::DataCorruption(format!("parsing memory {id}: {e}"))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::client;

    async fn test_db() -> (client::SurrealClient, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = client::connect(dir.path()).await.expect("connect");
        (db, dir)
    }

    #[tokio::test]
    async fn set_anchors_persists_field() {
        let (db, _dir) = test_db().await;

        let id = create(
            &db,
            MemoryDraft::new(
                "Sarah delivered the gRPC interceptor rollout".to_string(),
                crate::types::MemoryType::Semantic,
                crate::types::MemorySubtype::Fact,
                Some(crate::types::Channel::Cli),
                Some(crate::types::Modality::Text),
            ),
        )
        .await
        .expect("create");

        let anchors = vec![
            "Sarah".to_string(),
            "gRPC interceptor rollout".to_string(),
            "Rust".to_string(),
        ];
        set_anchors(&db, &id, &anchors).await.expect("set_anchors");

        let mem = get(&db, &id).await.expect("get");
        assert_eq!(mem.anchor_entities.as_deref(), Some(anchors.as_slice()));

        // Empty slice is a no-op — field should remain populated.
        set_anchors(&db, &id, &[]).await.expect("empty no-op");
        let mem2 = get(&db, &id).await.expect("get after empty");
        assert_eq!(
            mem2.anchor_entities.as_deref(),
            Some(anchors.as_slice()),
            "empty call must not clear existing anchors"
        );
    }
}
