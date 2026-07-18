//! Vector store output adapter — Phase 6.
//!
//! Writes [`EmbeddingRecord`] documents produced by the Phase 5 embedding
//! worker to MongoDB.  Two collections are used — one per [`EmbeddingKind`]:
//!
//! | Collection             | Key field       | Contents                           |
//! |------------------------|-----------------|------------------------------------|
//! | `content_embeddings`   | `embedding_id`  | OpenAI dense semantic vectors      |
//! | `behavioral_embeddings`| `embedding_id`  | Local structural feature vectors   |
//!
//! All writes are **idempotent**: re-running the pipeline for the same sample
//! replaces the existing record rather than inserting a duplicate.

use std::collections::HashSet;

use mongodb::bson::{self, doc, Document};
use mongodb::options::{IndexOptions, ReplaceOptions};
use mongodb::{Database, IndexModel};
use tokio::sync::Mutex;

use crate::embedding::{EmbeddingKind, EmbeddingRecord};
use crate::error::AppError;

// ─── Collection names ─────────────────────────────────────────────────────────

pub const CONTENT_EMBEDDINGS_COLL: &str = "content_embeddings";
pub const BEHAVIORAL_EMBEDDINGS_COLL: &str = "behavioral_embeddings";

// ─── VectorWriter ─────────────────────────────────────────────────────────────

/// Async writer that persists [`EmbeddingRecord`] documents to MongoDB.
///
/// Routing is determined by [`EmbeddingRecord::kind`]:
/// * [`EmbeddingKind::Content`]    → `content_embeddings`
/// * [`EmbeddingKind::Behavioral`] → `behavioral_embeddings`
///
/// # Example
/// ```rust,ignore
/// let writer = VectorWriter::new(repo.destination_db());
/// let records = embedding_result.into_records(&config.embedding.model);
/// writer.write(&records).await?;
/// ```
pub struct VectorWriter {
    db:      Database,
    indexed: Mutex<HashSet<String>>,
}

impl VectorWriter {
    /// Create a new writer backed by `db`.
    pub fn new(db: Database) -> Self {
        Self { db, indexed: Mutex::new(HashSet::new()) }
    }

    // ─── Public API ───────────────────────────────────────────────────────────

    /// Upsert [`EmbeddingRecord`] documents into the appropriate collection.
    ///
    /// Each record is matched by its stable `embedding_id` (UUID-v4), making
    /// the operation idempotent.  Content and behavioral records may be
    /// interleaved freely in the input slice.
    ///
    /// Returns the total number of records written (inserted or replaced).
    pub async fn write(&self, records: &[EmbeddingRecord]) -> Result<usize, AppError> {
        if records.is_empty() {
            return Ok(0);
        }

        // Ensure indexes for both collections up-front (no-op after first call).
        self.ensure_indexes(CONTENT_EMBEDDINGS_COLL).await?;
        self.ensure_indexes(BEHAVIORAL_EMBEDDINGS_COLL).await?;

        let content_col    = self.db.collection::<Document>(CONTENT_EMBEDDINGS_COLL);
        let behavioral_col = self.db.collection::<Document>(BEHAVIORAL_EMBEDDINGS_COLL);
        let opts           = ReplaceOptions::builder().upsert(true).build();
        let mut count      = 0usize;

        for record in records {
            let col = match record.kind {
                EmbeddingKind::Content    => &content_col,
                EmbeddingKind::Behavioral => &behavioral_col,
            };

            let doc = bson::to_document(record).map_err(|e| {
                AppError::Validation(format!("failed to serialize EmbeddingRecord: {e}"))
            })?;
            let filter = doc! { "embedding_id": &record.embedding_id };
            col.replace_one(filter, doc, opts.clone()).await?;
            count += 1;
        }
        Ok(count)
    }

    // ─── Index management ─────────────────────────────────────────────────────

    async fn ensure_indexes(&self, collection: &str) -> Result<(), AppError> {
        {
            let guard = self.indexed.lock().await;
            if guard.contains(collection) {
                return Ok(());
            }
        }

        let col = self.db.collection::<Document>(collection);

        // Unique index on embedding_id — the primary upsert key.
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "embedding_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some(format!("{collection}_unique_id")))
                        .unique(Some(true))
                        .build(),
                )
                .build(),
            None,
        ).await?;

        // sample_hash index — retrieve all embeddings for one sample quickly.
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "sample_hash": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some(format!("{collection}_sample_hash")))
                        .build(),
                )
                .build(),
            None,
        ).await?;

        // model index — filter by embedding model (e.g. "text-embedding-3-small").
        col.create_index(
            IndexModel::builder()
                .keys(doc! { "model": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some(format!("{collection}_model")))
                        .build(),
                )
                .build(),
            None,
        ).await?;

        let mut guard = self.indexed.lock().await;
        guard.insert(collection.to_string());
        Ok(())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use mongodb::bson::{self, DateTime};

    use super::*;
    use crate::embedding::{behavioral::BEHAVIORAL_DIM, EmbeddingKind, EmbeddingRecord};

    // ── Fixture helper ────────────────────────────────────────────────────────

    fn make_record(kind: EmbeddingKind, dims: usize, model: &str) -> EmbeddingRecord {
        EmbeddingRecord {
            embedding_id: format!("emb-uuid-{}", if kind == EmbeddingKind::Content { "content" } else { "beh" }),
            sample_hash:  "sha-abc".to_string(),
            kind,
            vector:       vec![0.1_f32; dims],
            model:        model.to_string(),
            dimensions:   dims as u32,
            created_at:   DateTime::now(),
        }
    }

    fn content_record() -> EmbeddingRecord {
        make_record(EmbeddingKind::Content, 1536, "text-embedding-3-small")
    }

    fn behavioral_record() -> EmbeddingRecord {
        make_record(EmbeddingKind::Behavioral, BEHAVIORAL_DIM, "behavioral-v1")
    }

    // ── Collection name constants ─────────────────────────────────────────────

    #[test]
    fn content_coll_name() {
        assert_eq!(CONTENT_EMBEDDINGS_COLL, "content_embeddings");
    }

    #[test]
    fn behavioral_coll_name() {
        assert_eq!(BEHAVIORAL_EMBEDDINGS_COLL, "behavioral_embeddings");
    }

    // ── EmbeddingRecord BSON serialization ────────────────────────────────────

    #[test]
    fn content_record_bson_field_set() {
        let rec = content_record();
        let doc = bson::to_document(&rec).expect("serialize failed");

        assert_eq!(doc.get_str("embedding_id").unwrap(), "emb-uuid-content");
        assert_eq!(doc.get_str("sample_hash").unwrap(), "sha-abc");
        assert_eq!(doc.get_str("model").unwrap(), "text-embedding-3-small");
        assert_eq!(doc.get_i32("dimensions").unwrap_or_else(|_| {
            doc.get_i64("dimensions").unwrap() as i32
        }), 1536);
        // kind serialises as snake_case → "content"
        assert_eq!(doc.get_str("kind").unwrap(), "content");
        assert!(doc.contains_key("vector"),   "vector field missing");
        assert!(doc.contains_key("created_at"), "created_at field missing");
    }

    #[test]
    fn behavioral_record_bson_field_set() {
        let rec = behavioral_record();
        let doc = bson::to_document(&rec).expect("serialize failed");

        assert_eq!(doc.get_str("kind").unwrap(), "behavioral");
        assert_eq!(doc.get_str("model").unwrap(), "behavioral-v1");
        let dims = doc.get_i32("dimensions")
            .unwrap_or_else(|_| doc.get_i64("dimensions").unwrap() as i32);
        assert_eq!(dims as usize, BEHAVIORAL_DIM);
    }

    #[test]
    fn record_vector_stored_as_bson_array() {
        let rec = make_record(EmbeddingKind::Content, 4, "test-model");
        let doc = bson::to_document(&rec).unwrap();
        let arr = doc.get_array("vector").expect("vector should be a BSON array");
        assert_eq!(arr.len(), 4, "vector length preserved");
    }

    #[test]
    fn record_vector_values_survive_f32_to_f64_bson_round_trip() {
        // BSON stores f32 as f64 Double — verify no catastrophic precision loss.
        let mut rec = make_record(EmbeddingKind::Behavioral, 3, "m");
        rec.vector = vec![0.1, 0.5, 0.9];
        let doc = bson::to_document(&rec).unwrap();
        let arr = doc.get_array("vector").unwrap();
        let values: Vec<f64> = arr
            .iter()
            .map(|b| b.as_f64().unwrap_or(f64::NAN))
            .collect();
        assert!((values[0] - 0.1_f64).abs() < 1e-5);
        assert!((values[1] - 0.5_f64).abs() < 1e-5);
        assert!((values[2] - 0.9_f64).abs() < 1e-5);
    }

    // ── Kind routing ──────────────────────────────────────────────────────────

    #[test]
    fn content_kind_routes_to_correct_collection_name() {
        let rec = content_record();
        let coll = match rec.kind {
            EmbeddingKind::Content    => CONTENT_EMBEDDINGS_COLL,
            EmbeddingKind::Behavioral => BEHAVIORAL_EMBEDDINGS_COLL,
        };
        assert_eq!(coll, "content_embeddings");
    }

    #[test]
    fn behavioral_kind_routes_to_correct_collection_name() {
        let rec = behavioral_record();
        let coll = match rec.kind {
            EmbeddingKind::Content    => CONTENT_EMBEDDINGS_COLL,
            EmbeddingKind::Behavioral => BEHAVIORAL_EMBEDDINGS_COLL,
        };
        assert_eq!(coll, "behavioral_embeddings");
    }

    // ── Deserialization round-trip ────────────────────────────────────────────

    #[test]
    fn embedding_record_bson_deserialize_round_trip_content() {
        let rec = content_record();
        let doc = bson::to_document(&rec).unwrap();
        let recovered: EmbeddingRecord = bson::from_document(doc).unwrap();

        assert_eq!(recovered.embedding_id, rec.embedding_id);
        assert_eq!(recovered.sample_hash,  rec.sample_hash);
        assert_eq!(recovered.kind,         rec.kind);
        assert_eq!(recovered.model,        rec.model);
        assert_eq!(recovered.dimensions,   rec.dimensions);
        assert_eq!(recovered.vector.len(), rec.vector.len());
    }

    #[test]
    fn embedding_record_bson_deserialize_round_trip_behavioral() {
        let rec = behavioral_record();
        let doc = bson::to_document(&rec).unwrap();
        let recovered: EmbeddingRecord = bson::from_document(doc).unwrap();

        assert_eq!(recovered.embedding_id, rec.embedding_id);
        assert_eq!(recovered.kind,         EmbeddingKind::Behavioral);
        assert_eq!(recovered.dimensions,   BEHAVIORAL_DIM as u32);
    }

    // ── dimensions invariant ──────────────────────────────────────────────────

    #[test]
    fn dimensions_field_matches_vector_length() {
        for dims in [64usize, 512, 1536] {
            let rec = make_record(EmbeddingKind::Content, dims, "m");
            assert_eq!(rec.dimensions as usize, rec.vector.len());
            let doc = bson::to_document(&rec).unwrap();
            let stored_dims = doc.get_i32("dimensions")
                .unwrap_or_else(|_| doc.get_i64("dimensions").unwrap() as i32)
                as usize;
            assert_eq!(stored_dims, dims);
        }
    }

    // ── Index name formatting ─────────────────────────────────────────────────

    #[test]
    fn index_names_include_collection_prefix() {
        let content_unique = format!("{CONTENT_EMBEDDINGS_COLL}_unique_id");
        let beh_sample     = format!("{BEHAVIORAL_EMBEDDINGS_COLL}_sample_hash");
        assert_eq!(content_unique, "content_embeddings_unique_id");
        assert_eq!(beh_sample,     "behavioral_embeddings_sample_hash");
    }
}
