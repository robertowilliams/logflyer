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
