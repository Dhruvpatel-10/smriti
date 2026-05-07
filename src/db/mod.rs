pub mod agent;
pub mod alerts;
pub mod auto_link;
pub mod chunking;
pub mod client;
pub mod consolidation;
pub mod consolidation_ledger;
pub mod dedup;
pub mod embeddings;
pub mod entity;
pub mod entity_alias;
pub mod export;
pub mod graph;
pub mod helpers;
pub mod markdown;
pub mod memory;
pub mod path_safety;
pub mod migrations;
pub mod prune;
pub mod recall_metrics;
pub mod reembed;
pub mod reranker;
pub mod search;
pub mod typed_relation;
pub mod validation;

/// Bitemporal predicate for "currently valid" memories — replaces `status = 'active'`
/// in all READ query paths.
///
/// Invariant (enforced at every write path by Stage B): `status = 'archived'` iff
/// `invalidated_at IS NOT NULL`, so checking `invalidated_at IS NONE` is equivalent
/// to `status = 'active'` for archived memories, AND additionally filters out
/// future-dated supersession via `valid_until` and TTL expiry via `expires_at`.
pub(crate) const BITEMPORAL_ACTIVE: &str =
    "invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now()) AND (expires_at IS NONE OR expires_at > time::now())";

/// Variant for write-path operations that process TTL expiry themselves.
/// Omits the `expires_at` guard so that memories whose TTL has elapsed are
/// still matched for archival by `prune::expire_ttl`.
pub(crate) const BITEMPORAL_ACTIVE_NO_TTL: &str =
    "invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())";
