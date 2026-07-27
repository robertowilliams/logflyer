//! Content-derived identifier helpers.
//!
//! Every ID emitted by the pipeline is a SHA-256 truncation over a stable
//! tuple of inputs.  Re-running the pipeline against the same sample produces
//! the *same* IDs, so the output adapters' `replace_one(filter, …, upsert)`
//! actually upserts instead of inserting a duplicate row.
//!
//! Why content-derived rather than UUID-v4:
//!
//! * Idempotency.  `entity_edges`, `prov_relations`, `otel_spans`, and the
//!   embedding collections all use these IDs as their composite or unique
//!   key.  Random IDs make every re-run a duplicate insert.
//!
//! * Cross-process determinism.  A backfill job and a live ingestion run
//!   processing the same sample emit the same IDs, so they don't fight each
//!   other for collection space.
//!
//! * Trivial joins.  The OTel `trace_id` is derivable from `sample_hash`
//!   alone, so external systems can compute the trace they want without
//!   needing to look up the sample first.
//!
//! Truncation lengths are chosen to match common conventions:
//!
//! | helper               | hex chars | bytes | matches                         |
//! |----------------------|-----------|-------|---------------------------------|
//! | `derive_trace_id`    | 32        | 16    | W3C OTel trace ID               |
//! | `derive_span_id`     | 16        | 8     | W3C OTel span ID                |
//! | `derive_entity_id`   | 32        | 16    | UUID-shaped opaque string       |
//! | `derive_relation_id` | 32        | 16    | UUID-shaped opaque string       |
//! | `derive_embedding_id`| 32        | 16    | UUID-shaped opaque string       |

use sha2::{Digest, Sha256};

/// Hash an arbitrary number of byte segments separated by a NUL terminator
/// (matches the convention used by [`crate::utils::compute_sample_hash`]).
fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0u8]);
        }
        hasher.update(p.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Truncate a hex digest to `n` characters.
fn truncate(s: String, n: usize) -> String {
    let mut s = s;
    s.truncate(n);
    s
}

/// W3C-OTel-compatible 32-hex-char trace ID derived from `sample_hash`.
///
/// Stable for the lifetime of the sample — the same sample hash always
/// produces the same trace id, regardless of how many times the pipeline
/// is run.
pub fn derive_trace_id(sample_hash: &str) -> String {
    truncate(sha256_hex(&[sample_hash, "trace"]), 32)
}

/// W3C-OTel-compatible 16-hex-char span ID for `(sample_hash, line_index)`.
pub fn derive_span_id(sample_hash: &str, line_index: u32) -> String {
    truncate(sha256_hex(&[sample_hash, "span", &line_index.to_string()]), 16)
}

/// 32-hex-char task ID derived from a correlation key.
///
/// **Deliberately not keyed on `sample_hash`.** This is the one id in the module
/// that is meant to be *shared* across samples: two samples carrying the same
/// `session_id` must produce the same task id, which is how a task spanning
/// several log samples gets stitched back together.
///
/// `key_name` is part of the hash so that `session_id=abc` and `run_id=abc`
/// cannot collide — different correlation dimensions that happen to share a value
/// are different tasks.
///
/// When a log carries no correlation key at all, the caller passes
/// `("sample", sample_hash)`, which reproduces today's one-task-per-sample
/// behaviour rather than merging unrelated work.
///
/// # `scope` — why some values must not be global
///
/// Sharing the id across samples is only safe when the value is *globally*
/// unique. `session_id=8f3c1e7a-…` is. `run_id=1` is not: every system that
/// numbers its runs from one emits it, and hashing that unscoped merges
/// unrelated work from unrelated sources into a single task, which is precisely
/// the failure this id exists to avoid.
///
/// So the caller passes `Some(target_id)` for low-entropy values. Grouping still
/// works within one target — run 1 of a target reassembles across all its
/// samples — but run 1 over here no longer collides with run 1 over there.
/// High-entropy values pass `None` and stay global, so a task genuinely observed
/// across two sources still stitches together.
///
/// See [`super::task_correlator::is_globally_unique`] for the entropy test.
pub fn derive_task_id(key_name: &str, key_value: &str, scope: Option<&str>) -> String {
    match scope {
        Some(target) => truncate(sha256_hex(&[key_name, "task", target, key_value]), 32),
        None => truncate(sha256_hex(&[key_name, "task", key_value]), 32),
    }
}

/// 32-hex-char actor ID derived from `(kind, name)`.
///
/// Like [`derive_task_id`] and unlike the event ids, this is **deliberately not
/// keyed on `sample_hash`** — the model `claude-opus-4` is the same agent
/// wherever it appears, and the tool `web_search` is the same skill. Sharing the
/// id across samples is what makes "which agents used this skill" answerable at
/// all; a per-sample actor id would produce one disconnected node per occurrence.
///
/// `kind` is part of the hash so an agent and a skill that happen to share a name
/// remain distinct nodes.
pub fn derive_actor_id(kind: &str, name: &str) -> String {
    truncate(sha256_hex(&[kind, "actor", name]), 32)
}

/// 32-hex-char opaque entity ID derived from the sample's stable identity
/// plus the line that produced this entity.  `raw_text` is included so two
/// entities extracted from the same line by different rules can still get
/// distinct IDs (currently impossible, but cheap insurance).
pub fn derive_entity_id(sample_hash: &str, line_index: u32, raw_text: &str) -> String {
    truncate(
        sha256_hex(&[sample_hash, "entity", &line_index.to_string(), raw_text]),
        32,
    )
}

/// 32-hex-char opaque relation ID derived from `(sample_hash, type, src, dst)`.
///
/// The relation type's `Debug` rendering (`"TriggeredBy"`, `"Generated"`, …)
/// is used as the type discriminator — it's stable across compilation units
/// and changes only when an enum variant is renamed (which is itself a
/// breaking change requiring a backfill).
pub fn derive_relation_id(
    sample_hash: &str,
    relation_type_debug: &str,
    source_entity_id: &str,
    target_entity_id: &str,
) -> String {
    truncate(
        sha256_hex(&[
            sample_hash,
            "rel",
            relation_type_debug,
            source_entity_id,
            target_entity_id,
        ]),
        32,
    )
}

/// 32-hex-char opaque embedding ID derived from `(sample_hash, kind, model)`.
///
/// `kind` is the canonical serde string ("content" / "behavioral") so the
/// IDs survive a model swap by changing — re-embedding with a new model
/// inserts a new row rather than overwriting the old vector.  Same model,
/// same sample, same kind ⇒ same id ⇒ idempotent upsert.
pub fn derive_embedding_id(sample_hash: &str, kind: &str, model: &str) -> String {
    truncate(sha256_hex(&[sample_hash, "emb", kind, model]), 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_trace_id_is_deterministic() {
        assert_eq!(derive_trace_id("abc"), derive_trace_id("abc"));
        assert_ne!(derive_trace_id("abc"), derive_trace_id("abd"));
        assert_eq!(derive_trace_id("abc").len(), 32);
    }

    #[test]
    fn derive_span_id_distinguishes_lines() {
        assert_eq!(derive_span_id("abc", 0), derive_span_id("abc", 0));
        assert_ne!(derive_span_id("abc", 0), derive_span_id("abc", 1));
        assert_eq!(derive_span_id("abc", 0).len(), 16);
    }

    #[test]
    fn derive_task_id_is_deterministic() {
        assert_eq!(
            derive_task_id("session_id", "sess-abc", None),
            derive_task_id("session_id", "sess-abc", None),
        );
        assert_eq!(derive_task_id("session_id", "sess-abc", None).len(), 32);
    }

    #[test]
    fn derive_task_id_is_independent_of_the_sample() {
        // The defining property: the same correlation key must yield the same task
        // id no matter which sample it was seen in. This is what lets a task span
        // several samples.
        let from_one_sample = derive_task_id("session_id", "sess-abc", None);
        let from_another = derive_task_id("session_id", "sess-abc", None);
        assert_eq!(from_one_sample, from_another);
    }

    #[test]
    fn derive_task_id_separates_key_dimensions() {
        // `session_id=abc` and `run_id=abc` are different tasks that happen to
        // share a value; they must not collide.
        assert_ne!(
            derive_task_id("session_id", "abc", None),
            derive_task_id("run_id", "abc", None),
        );
    }

    #[test]
    fn derive_task_id_distinguishes_values() {
        assert_ne!(
            derive_task_id("session_id", "sess-a", None),
            derive_task_id("session_id", "sess-b", None),
        );
    }

    #[test]
    fn derive_task_id_sample_fallback_is_per_sample() {
        // With no correlation key, each sample is its own task — today's behaviour.
        assert_ne!(
            derive_task_id("sample", "hash-a", None),
            derive_task_id("sample", "hash-b", None),
        );
    }

    #[test]
    fn derive_task_id_scope_separates_low_entropy_values_across_targets() {
        // The point of the scope: `run_id=1` from two unrelated systems is two
        // tasks, not one. Without it they hash identically and merge.
        assert_ne!(
            derive_task_id("run_id", "1", Some("target-a")),
            derive_task_id("run_id", "1", Some("target-b")),
        );
        // ...while staying stable within one target, so a task still reassembles
        // across that target's samples.
        assert_eq!(
            derive_task_id("run_id", "1", Some("target-a")),
            derive_task_id("run_id", "1", Some("target-a")),
        );
        // And a scoped id is never the unscoped one.
        assert_ne!(
            derive_task_id("run_id", "1", Some("target-a")),
            derive_task_id("run_id", "1", None),
        );
    }

    #[test]
    fn derive_task_id_does_not_collide_with_the_trace_id() {
        // Both are 32 hex chars over the same input in the fallback case, so the
        // domain separator has to keep them apart — otherwise a task id and a
        // trace id could be confused for one another.
        assert_ne!(derive_task_id("sample", "abc", None), derive_trace_id("abc"));
    }

    #[test]
    fn derive_actor_id_is_shared_across_samples() {
        // The defining property: the same actor is one node everywhere, which is
        // what makes cross-sample "who used what" queries possible.
        assert_eq!(
            derive_actor_id("agent", "claude-opus-4"),
            derive_actor_id("agent", "claude-opus-4"),
        );
        assert_eq!(derive_actor_id("agent", "claude-opus-4").len(), 32);
    }

    #[test]
    fn derive_actor_id_separates_kinds() {
        // An agent and a skill sharing a name must not collapse into one node.
        assert_ne!(
            derive_actor_id("agent", "search"),
            derive_actor_id("skill", "search"),
        );
    }

    #[test]
    fn derive_actor_id_distinguishes_names() {
        assert_ne!(
            derive_actor_id("skill", "web_search"),
            derive_actor_id("skill", "file_writer"),
        );
    }

    #[test]
    fn derive_actor_id_does_not_collide_with_other_id_kinds() {
        // All these are 32 hex chars; the domain separators must keep them apart.
        let name = "abc";
        let actor = derive_actor_id("agent", name);
        assert_ne!(actor, derive_task_id("agent", name, None));
        assert_ne!(actor, derive_trace_id(name));
    }

    #[test]
    fn derive_entity_id_distinguishes_text() {
        let a = derive_entity_id("h", 0, "line one");
        let b = derive_entity_id("h", 0, "line two");
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn derive_relation_id_is_directional() {
        let a = derive_relation_id("h", "Generated", "src", "dst");
        let b = derive_relation_id("h", "Generated", "dst", "src");
        assert_ne!(a, b, "relation IDs must distinguish direction");
    }

    #[test]
    fn derive_embedding_id_distinguishes_kind_and_model() {
        let cm1 = derive_embedding_id("h", "content", "text-embedding-3-small");
        let cm2 = derive_embedding_id("h", "content", "text-embedding-3-large");
        let bm  = derive_embedding_id("h", "behavioral", "text-embedding-3-small");
        assert_ne!(cm1, cm2, "model swap should change id");
        assert_ne!(cm1, bm,  "kind swap should change id");
    }
}
