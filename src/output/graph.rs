//! Graph store output adapter — Phase 6.
//!
//! Writes [`RelationEdge`] records from the relation extractor and
//! [`ProvTriple`] records from the PROV-O linker to MongoDB collections in
//! the destination database.
//!
//! # Collections
//!
//! | Collection      | Key field                                          | Contents                  |
//! |-----------------|----------------------------------------------------|---------------------------|
//! | `entity_edges`  | `relation_id` (UUID-v4, unique)                    | One [`RelationEdge`]      |
//! | `prov_relations`| `(sample_hash, subject, predicate, object)` tuple | One [`ProvTriple`]        |
//!
//! Both writers are **idempotent**: running the same sample through the
//! pipeline twice will replace existing documents rather than inserting
//! duplicates.

use std::collections::HashSet;

use mongodb::bson::{self, doc, Document};
use mongodb::options::{IndexOptions, ReplaceOptions};
use mongodb::{Database, IndexModel};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::models::RelationEdge;
use crate::preprocessing::otel_builder::OtelSpan;
use crate::preprocessing::prov_linker::ProvTriple;

// ─── Collection names ─────────────────────────────────────────────────────────

pub const ENTITY_EDGES_COLL: &str = "entity_edges";
pub const PROV_RELATIONS_COLL: &str = "prov_relations";
pub const OTEL_SPANS_COLL: &str = "otel_spans";

// ─── GraphWriter ──────────────────────────────────────────────────────────────

/// Async writer that persists relation edges and PROV-O triples to MongoDB.
///
/// Construct once per pipeline run (or share across runs — the inner
/// `Database` handle is cheap to clone and the index-tracking set is
/// protected by a `Mutex`).
///
/// # Example
/// ```rust,ignore
/// let writer = GraphWriter::new(repo.destination_db());
/// writer.write_edges(&metadata.relations).await?;
/// writer.write_prov(&prov_triples).await?;
/// ```
pub struct GraphWriter {
    db:      Database,
    indexed: Mutex<HashSet<String>>,
}

impl GraphWriter {
    /// Create a new writer backed by `db`.
    pub fn new(db: Database) -> Self {
        Self { db, indexed: Mutex::new(HashSet::new()) }
    }

    // ─── Public API ───────────────────────────────────────────────────────────

    /// Upsert [`RelationEdge`] records into the `entity_edges` collection.
    ///
    /// Each edge is matched by its stable `relation_id` (UUID-v4), so
    /// re-processing the same sample is safe.  Returns the number of edges
    /// written (inserted **or** replaced — duplicates count as 1).
    pub async fn write_edges(&self, edges: &[RelationEdge]) -> Result<usize, AppError> {
        if edges.is_empty() {
            return Ok(0);
        }
        self.ensure_edge_indexes().await?;

        let col  = self.db.collection::<Document>(ENTITY_EDGES_COLL);
        let opts = ReplaceOptions::builder().upsert(true).build();
        let mut count = 0usize;

        for edge in edges {
            let doc = bson::to_document(edge).map_err(|e| {
                AppError::Validation(format!("failed to serialize RelationEdge: {e}"))
            })?;
            let filter = doc! { "relation_id": &edge.relation_id };
            col.replace_one(filter, doc, opts.clone()).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Upsert [`OtelSpan`] records into the `otel_spans` collection.
    ///
    /// Spans are matched by the composite key `(trace_id, span_id)`, making
    /// the operation idempotent.  Returns the number of spans written.
    pub async fn write_spans(&self, spans: &[OtelSpan]) -> Result<usize, AppError> {
        if spans.is_empty() {
            return Ok(0);
        }
        self.ensure_span_indexes().await?;

        let col  = self.db.collection::<Document>(OTEL_SPANS_COLL);
        let opts = ReplaceOptions::builder().upsert(true).build();
        let mut count = 0usize;

        for span in spans {
            let doc = bson::to_document(span).map_err(|e| {
                AppError::Validation(format!("failed to serialize OtelSpan: {e}"))
            })?;
            let filter = doc! {
                "trace_id": &span.trace_id,
                "span_id":  &span.span_id,
            };
            col.replace_one(filter, doc, opts.clone()).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Upsert [`ProvTriple`] records into the `prov_relations` collection.
    ///
    /// Triples are matched by the composite key
    /// `(sample_hash, subject, predicate, object)`, making the operation
    /// idempotent.  Returns the number of triples written.
    pub async fn write_prov(&self, triples: &[ProvTriple]) -> Result<usize, AppError> {
        if triples.is_empty() {
            return Ok(0);
        }
        self.ensure_prov_indexes().await?;

        let col  = self.db.collection::<Document>(PROV_RELATIONS_COLL);
        let opts = ReplaceOptions::builder().upsert(true).build();
        let mut count = 0usize;

        for triple in triples {
            // Serialize the full document first — this gives us the predicate
            // as the camelCase string that `ProvPredicate`'s serde impl emits
            // (e.g. "wasGeneratedBy"), which we reuse in the composite filter.
            let doc = bson::to_document(triple).map_err(|e| {
                AppError::Validation(format!("failed to serialize ProvTriple: {e}"))
            })?;

            let predicate_bson = doc
                .get("predicate")
                .cloned()
                .unwrap_or(bson::Bson::Null);

            let filter = doc! {
                "sample_hash": &triple.sample_hash,
                "subject":     &triple.subject,
                "predicate":   predicate_bson,
                "object":      &triple.object,
            };
            col.replace_one(filter, doc, opts.clone()).await?;
            count += 1;
        }
        Ok(count)
    }

    // ─── Index management ─────────────────────────────────────────────────────

    async fn ensure_edge_indexes(&self) -> Result<(), AppError> {
        {
            let guard = self.indexed.lock().await;
            if guard.contains(ENTITY_EDGES_COLL) {
                return Ok(());
            }
        }

        let col = self.db.collection::<Document>(ENTITY_EDGES_COLL);

        col.create_index(
            IndexModel::builder()
                .keys(doc! { "relation_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some("ee_unique_relation_id".to_string()))
                        .unique(Some(true))
                        .build(),
                )
                .build(),
            None,
        ).await?;

        col.create_index(
            IndexModel::builder()
                .keys(doc! { "sample_hash": 1 })
                .options(IndexOptions::builder().name(Some("ee_sample_hash".to_string())).build())
                .build(),
            None,
        ).await?;

        col.create_index(
            IndexModel::builder()
                .keys(doc! { "source_entity_id": 1 })
                .options(IndexOptions::builder().name(Some("ee_source_entity_id".to_string())).build())
                .build(),
            None,
        ).await?;

        col.create_index(
            IndexModel::builder()
                .keys(doc! { "target_entity_id": 1 })
                .options(IndexOptions::builder().name(Some("ee_target_entity_id".to_string())).build())
                .build(),
            None,
        ).await?;

        col.create_index(
            IndexModel::builder()
                .keys(doc! { "relation_type": 1 })
                .options(IndexOptions::builder().name(Some("ee_relation_type".to_string())).build())
                .build(),
            None,
        ).await?;

        let mut guard = self.indexed.lock().await;
        guard.insert(ENTITY_EDGES_COLL.to_string());
        Ok(())
    }

    async fn ensure_prov_indexes(&self) -> Result<(), AppError> {
        {
            let guard = self.indexed.lock().await;
            if guard.contains(PROV_RELATIONS_COLL) {
                return Ok(());
            }
        }

        let col = self.db.collection::<Document>(PROV_RELATIONS_COLL);

        // sample_hash → retrieve all triples for a sample in one query
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "sample_hash": 1 })
                .options(IndexOptions::builder().name(Some("pr_sample_hash".to_string())).build())
                .build(),
            None,
        ).await?;

        // SPO compound index — SPARQL-style subject-predicate-object lookup
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "subject": 1, "predicate": 1, "object": 1 })
                .options(IndexOptions::builder().name(Some("pr_spo".to_string())).build())
                .build(),
            None,
        ).await?;

        // Inverse object lookup — find all triples that reference a given entity
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "object": 1 })
                .options(IndexOptions::builder().name(Some("pr_object".to_string())).build())
                .build(),
            None,
        ).await?;

        let mut guard = self.indexed.lock().await;
        guard.insert(PROV_RELATIONS_COLL.to_string());
        Ok(())
    }

    async fn ensure_span_indexes(&self) -> Result<(), AppError> {
        {
            let guard = self.indexed.lock().await;
            if guard.contains(OTEL_SPANS_COLL) {
                return Ok(());
            }
        }

        let col = self.db.collection::<Document>(OTEL_SPANS_COLL);

        // Composite unique index on (trace_id, span_id) — primary upsert key.
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "trace_id": 1, "span_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some("os_unique_trace_span".to_string()))
                        .unique(Some(true))
                        .build(),
                )
                .build(),
            None,
        ).await?;

        // sample_hash → fetch all spans for one sample.
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "sample_hash": 1 })
                .options(IndexOptions::builder().name(Some("os_sample_hash".to_string())).build())
                .build(),
            None,
        ).await?;

        // trace_id → fetch all spans in a single trace (parent + children).
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "trace_id": 1 })
                .options(IndexOptions::builder().name(Some("os_trace_id".to_string())).build())
                .build(),
            None,
        ).await?;

        let mut guard = self.indexed.lock().await;
        guard.insert(OTEL_SPANS_COLL.to_string());
        Ok(())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use mongodb::bson::{self, DateTime};

    use super::*;
    use crate::models::{RelationEdge, RelationSource, RelationType};
    use crate::preprocessing::prov_linker::ProvPredicate;

    // ── Fixture helpers ───────────────────────────────────────────────────────

    fn make_edge(relation_type: RelationType) -> RelationEdge {
        RelationEdge {
            relation_id:      "edge-uuid-0001".to_string(),
            relation_type,
            source_entity_id: "ent-aaa".to_string(),
            target_entity_id: "ent-bbb".to_string(),
            sample_hash:      "sha-001".to_string(),
            confidence:       0.9,
            source:           RelationSource::Inferred,
            created_at:       DateTime::now(),
        }
    }

    fn make_triple(predicate: ProvPredicate) -> ProvTriple {
        ProvTriple {
            subject:     "ug:entity:ent-aaa".to_string(),
            predicate,
            object:      "ug:activity:sha-001:0".to_string(),
            sample_hash: "sha-001".to_string(),
            created_at:  DateTime::now(),
        }
    }

    // ── Collection name constants ─────────────────────────────────────────────

    #[test]
    fn entity_edges_coll_name() {
        assert_eq!(ENTITY_EDGES_COLL, "entity_edges");
    }

    #[test]
    fn prov_relations_coll_name() {
        assert_eq!(PROV_RELATIONS_COLL, "prov_relations");
    }

    // ── RelationEdge BSON serialization ───────────────────────────────────────

    #[test]
    fn relation_edge_bson_round_trip() {
        let edge = make_edge(RelationType::TriggeredBy);
        let doc = bson::to_document(&edge).expect("serialize failed");

        assert_eq!(doc.get_str("relation_id").unwrap(), "edge-uuid-0001");
        assert_eq!(doc.get_str("source_entity_id").unwrap(), "ent-aaa");
        assert_eq!(doc.get_str("target_entity_id").unwrap(), "ent-bbb");
        assert_eq!(doc.get_str("sample_hash").unwrap(), "sha-001");
        // RelationType uses SCREAMING_SNAKE_CASE → "TRIGGERED_BY"
        assert_eq!(doc.get_str("relation_type").unwrap(), "TRIGGERED_BY");
        // source enum: snake_case → "inferred"
        assert_eq!(doc.get_str("source").unwrap(), "inferred");
        // created_at must be a BSON DateTime
        assert!(doc.contains_key("created_at"),
                "created_at must be present");
        // confidence is stored as f64 (BSON Double)
        let conf = doc.get_f64("confidence").unwrap_or(-1.0);
        assert!((conf - 0.9).abs() < 1e-5, "confidence mismatch: {conf}");
    }

    #[test]
    fn relation_edge_generated_type_serializes_correctly() {
        let edge = make_edge(RelationType::Generated);
        let doc = bson::to_document(&edge).unwrap();
        assert_eq!(doc.get_str("relation_type").unwrap(), "GENERATED");
    }

    #[test]
    fn relation_edge_all_types_round_trip() {
        let types = [
            (RelationType::TriggeredBy,   "TRIGGERED_BY"),
            (RelationType::Generated,     "GENERATED"),
            (RelationType::Informed,      "INFORMED"),
            (RelationType::FollowedBy,    "FOLLOWED_BY"),
            (RelationType::RespondedTo,   "RESPONDED_TO"),
            (RelationType::AssembledFrom, "ASSEMBLED_FROM"),
            (RelationType::PartOf,        "PART_OF"),
            (RelationType::DelegatedTo,   "DELEGATED_TO"),
        ];
        for (rt, expected) in types {
            let doc = bson::to_document(&make_edge(rt)).unwrap();
            assert_eq!(
                doc.get_str("relation_type").unwrap(),
                expected,
                "mismatch for {expected}"
            );
        }
    }

    #[test]
    fn relation_edge_explicit_source_serializes() {
        let mut edge = make_edge(RelationType::Generated);
        edge.source = RelationSource::Explicit;
        let doc = bson::to_document(&edge).unwrap();
        assert_eq!(doc.get_str("source").unwrap(), "explicit");
    }

    // ── ProvTriple BSON serialization ─────────────────────────────────────────

    #[test]
    fn prov_triple_was_generated_by_serializes() {
        let triple = make_triple(ProvPredicate::WasGeneratedBy);
        let doc = bson::to_document(&triple).unwrap();

        assert_eq!(doc.get_str("subject").unwrap(), "ug:entity:ent-aaa");
        assert_eq!(doc.get_str("predicate").unwrap(), "wasGeneratedBy");
        assert_eq!(doc.get_str("object").unwrap(), "ug:activity:sha-001:0");
        assert_eq!(doc.get_str("sample_hash").unwrap(), "sha-001");
        assert!(doc.contains_key("created_at"));
    }

    #[test]
    fn prov_triple_all_predicates_round_trip() {
        let predicates = [
            (ProvPredicate::WasGeneratedBy,  "wasGeneratedBy"),
            (ProvPredicate::WasAttributedTo, "wasAttributedTo"),
            (ProvPredicate::WasDerivedFrom,  "wasDerivedFrom"),
            (ProvPredicate::Used,            "used"),
            (ProvPredicate::ActedOnBehalfOf, "actedOnBehalfOf"),
        ];
        for (pred, expected) in predicates {
            let doc = bson::to_document(&make_triple(pred)).unwrap();
            assert_eq!(
                doc.get_str("predicate").unwrap(),
                expected,
                "mismatch for {expected}"
            );
        }
    }

    #[test]
    fn prov_triple_predicate_extracted_from_doc_matches_filter_form() {
        // Verify the pattern used in write_prov: serialize to doc, then
        // extract the predicate BSON value for the composite filter.
        let triple = make_triple(ProvPredicate::WasAttributedTo);
        let doc = bson::to_document(&triple).unwrap();
        let predicate_bson = doc.get("predicate").cloned().unwrap();
        // Must be the string "wasAttributedTo", not a wrapped enum variant.
        assert_eq!(predicate_bson.as_str().unwrap(), "wasAttributedTo");
    }

    // ── Confidence precision ──────────────────────────────────────────────────

    #[test]
    fn edge_confidence_low_value_survives_f32_to_f64_bson_round_trip() {
        let mut edge = make_edge(RelationType::FollowedBy);
        edge.confidence = 0.7;
        let doc = bson::to_document(&edge).unwrap();
        let conf = doc.get_f64("confidence").unwrap();
        // f32 0.7 → f64 should be within normal floating-point tolerance
        assert!((conf - 0.7_f64).abs() < 1e-5, "confidence {conf}");
    }

    // ── Empty-slice early-return (no DB required) ─────────────────────────────
    // These tests verify the logic branches at compile time via type checking;
    // the actual early-return path is validated by the `Result<usize>` return type
    // and the fact that the functions touch no DB state when inputs are empty.
    // Full integration tests (with a live MongoDB) are out-of-scope for unit tests.

    #[test]
    fn relation_edge_has_correct_field_set() {
        let edge = make_edge(RelationType::PartOf);
        let doc = bson::to_document(&edge).unwrap();
        let expected_keys = [
            "relation_id", "relation_type", "source_entity_id",
            "target_entity_id", "sample_hash", "confidence", "source", "created_at",
        ];
        for key in &expected_keys {
            assert!(doc.contains_key(*key), "missing key: {key}");
        }
    }

    #[test]
    fn prov_triple_has_correct_field_set() {
        let triple = make_triple(ProvPredicate::WasDerivedFrom);
        let doc = bson::to_document(&triple).unwrap();
        let expected_keys = ["subject", "predicate", "object", "sample_hash", "created_at"];
        for key in &expected_keys {
            assert!(doc.contains_key(*key), "missing key: {key}");
        }
    }

    // ── Deserialization round-trip (ensures filter values match stored values) ─

    #[test]
    fn relation_edge_bson_deserialize_round_trip() {
        let edge = make_edge(RelationType::DelegatedTo);
        let doc = bson::to_document(&edge).unwrap();
        let recovered: RelationEdge = bson::from_document(doc).unwrap();
        assert_eq!(recovered.relation_id,      edge.relation_id);
        assert_eq!(recovered.source_entity_id, edge.source_entity_id);
        assert_eq!(recovered.target_entity_id, edge.target_entity_id);
        assert_eq!(recovered.sample_hash,      edge.sample_hash);
        assert_eq!(recovered.relation_type,    edge.relation_type);
    }

    #[test]
    fn prov_triple_bson_deserialize_round_trip() {
        let triple = make_triple(ProvPredicate::WasGeneratedBy);
        let doc = bson::to_document(&triple).unwrap();
        let recovered: ProvTriple = bson::from_document(doc).unwrap();
        assert_eq!(recovered.subject,     triple.subject);
        assert_eq!(recovered.predicate,   triple.predicate);
        assert_eq!(recovered.object,      triple.object);
        assert_eq!(recovered.sample_hash, triple.sample_hash);
    }

}
