use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mongodb::bson::{self, doc, Bson, DateTime, Document};
use mongodb::error::Error as MongoError;
use mongodb::options::{
    ClientOptions, FindOneAndReplaceOptions, FindOneOptions, FindOptions, IndexOptions,
    ReplaceOptions, UpdateOptions,
};
use mongodb::{Client, Collection, Database, IndexModel};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tracing::warn;

use super::graph_query::{
    shortest_path, Direction, EdgeEndpoints, PathHop, Traversal, MAX_EDGES,
    STRUCTURAL_RELATION_TYPES,
};
use super::vector_query::{rank, Candidate, ScoredHit, MAX_SCAN};
use crate::config::{ConfigHistoryConfig, MongoConfig};
use crate::config_history;
use crate::embedding::EmbeddingKind;
use crate::error::AppError;
use crate::preprocessing::task_correlator::SAMPLE_FALLBACK;
use crate::output::vector::{
    BEHAVIORAL_EMBEDDINGS_COLL, CONTENT_EMBEDDINGS_COLL, TASK_EMBEDDINGS_COLL,
};
use crate::models::{
    ActorRecord, ClassificationRecord, ClassificationStatus, SampleMetadata, SampleRecord,
};

/// The task index collection (Stage 11).
pub const TASKS_COLL: &str = "tasks";

/// The actor node collection (Stage 12).
pub const ACTORS_COLL: &str = "actors";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    Inserted,
    Duplicate,
}

#[derive(Clone)]
pub struct MongoRepository {
    client: Client,
    source_database: Database,
    destination_database: Database,
    tracking_database: Database,
    source_collection_name: String,
    tracking_collection_name: String,
    indexed_collections: Arc<Mutex<HashSet<String>>>,
}

impl MongoRepository {
    pub async fn connect(config: &MongoConfig) -> Result<Self, AppError> {
        let mut options = ClientOptions::parse(&config.uri).await?;
        options.app_name = Some("logflayer".to_string());

        let client = Client::with_options(options)?;
        let source_database = client.database(&config.source_db_name);
        let destination_database = client.database(&config.destination_db_name);
        let tracking_database = client.database(&config.tracking_db_name);

        Ok(Self {
            client,
            source_database,
            destination_database,
            tracking_database,
            source_collection_name: config.source_collection_name.clone(),
            tracking_collection_name: config.tracking_collection_name.clone(),
            indexed_collections: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    pub async fn ping(&self) -> Result<(), AppError> {
        self.client
            .database("admin")
            .run_command(doc! {"ping": 1}, None)
            .await?;
        Ok(())
    }

    // ─── Sampling service methods ─────────────────────────────────────────────

    pub async fn fetch_active_targets(&self) -> Result<Vec<Document>, AppError> {
        let collection = self
            .source_database
            .collection::<Document>(&self.source_collection_name);
        let mut cursor = collection.find(doc! {"status": "active"}, None).await?;
        let mut documents = Vec::new();
        while cursor.advance().await? {
            documents.push(cursor.deserialize_current()?);
        }
        Ok(documents)
    }

    pub async fn store_sample(
        &self,
        collection_name: &str,
        sample: &SampleRecord,
    ) -> Result<StoreOutcome, AppError> {
        self.ensure_indexes(collection_name).await?;
        let collection = self.destination_collection(collection_name);
        match collection.insert_one(sample.to_document(), None).await {
            Ok(_) => Ok(StoreOutcome::Inserted),
            Err(error) if is_duplicate_key_error(&error) => Ok(StoreOutcome::Duplicate),
            Err(error) => Err(AppError::Mongo(error)),
        }
    }

    async fn ensure_indexes(&self, collection_name: &str) -> Result<(), AppError> {
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains(collection_name) {
                return Ok(());
            }
        }
        let collection = self.destination_collection(collection_name);
        let unique_hash_index = IndexModel::builder()
            .keys(doc! { "sample_hash": 1 })
            .options(
                IndexOptions::builder()
                    .name(Some("unique_sample_hash".to_string()))
                    .unique(Some(true))
                    .build(),
            )
            .build();
        let timestamp_index = IndexModel::builder()
            .keys(doc! { "timestamp": -1, "source_file": 1 })
            .options(
                IndexOptions::builder()
                    .name(Some("recent_samples".to_string()))
                    .build(),
            )
            .build();
        collection.create_index(unique_hash_index, None).await?;
        collection.create_index(timestamp_index, None).await?;
        let mut guard = self.indexed_collections.lock().await;
        guard.insert(collection_name.to_string());
        Ok(())
    }

    pub async fn store_metadata(&self, metadata: &SampleMetadata) -> Result<(), AppError> {
        self.ensure_metadata_indexes().await?;
        let document = metadata.to_document()?;
        let filter = doc! { "sample_hash": &metadata.sample_hash };
        let options = ReplaceOptions::builder().upsert(true).build();
        self.destination_database
            .collection::<Document>("sample_metadata")
            .replace_one(filter, document, options)
            .await?;
        Ok(())
    }

    pub async fn fetch_unprocessed_samples(
        &self,
        limit: usize,
    ) -> Result<Vec<SampleRecord>, AppError> {
        let processed_hashes: HashSet<String> = {
            let meta_col = self
                .destination_database
                .collection::<Document>("sample_metadata");
            let opts = FindOptions::builder()
                .projection(doc! { "sample_hash": 1, "_id": 0 })
                .build();
            let mut cursor = meta_col.find(doc! {}, opts).await?;
            let mut hashes = HashSet::new();
            while cursor.advance().await? {
                let doc = cursor.deserialize_current()?;
                if let Some(Bson::String(hash)) = doc.get("sample_hash") {
                    hashes.insert(hash.clone());
                }
            }
            hashes
        };

        let collection_names = self.destination_database.list_collection_names(None).await?;
        let mut unprocessed = Vec::new();

        'outer: for name in collection_names {
            if name == "sample_metadata" {
                continue;
            }
            let col = self.destination_database.collection::<Document>(&name);
            let mut cursor = col
                .find(
                    doc! {},
                    FindOptions::builder()
                        .sort(doc! { "timestamp": -1 })
                        .build(),
                )
                .await?;

            while cursor.advance().await? {
                if unprocessed.len() >= limit {
                    break 'outer;
                }
                let document = cursor.deserialize_current()?;
                let hash = match document.get("sample_hash") {
                    Some(Bson::String(h)) => h.clone(),
                    _ => continue,
                };
                if processed_hashes.contains(&hash) {
                    continue;
                }
                if let Ok(record) = bson::from_document::<SampleRecord>(document) {
                    unprocessed.push(record);
                }
            }
        }
        Ok(unprocessed)
    }

    async fn ensure_metadata_indexes(&self) -> Result<(), AppError> {
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains("sample_metadata") {
                return Ok(());
            }
        }
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");
        let unique_hash = IndexModel::builder()
            .keys(doc! { "sample_hash": 1 })
            .options(
                IndexOptions::builder()
                    .name(Some("unique_metadata_hash".to_string()))
                    .unique(Some(true))
                    .build(),
            )
            .build();
        let target_time = IndexModel::builder()
            .keys(doc! { "target_id": 1, "analyzed_at": -1 })
            .options(
                IndexOptions::builder()
                    .name(Some("metadata_target_time".to_string()))
                    .build(),
            )
            .build();
        let worth_classifying = IndexModel::builder()
            .keys(doc! { "ingestion_hints.worth_classifying": 1 })
            .options(
                IndexOptions::builder()
                    .name(Some("metadata_worth_classifying".to_string()))
                    .build(),
            )
            .build();
        col.create_index(unique_hash, None).await?;
        col.create_index(target_time, None).await?;
        col.create_index(worth_classifying, None).await?;
        let mut guard = self.indexed_collections.lock().await;
        guard.insert("sample_metadata".to_string());
        Ok(())
    }

    pub async fn delete_stale_metadata(&self, current_version: &str) -> Result<u64, AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");
        let filter = doc! { "preprocessing_version": { "$ne": current_version } };
        let result = col.delete_many(filter, None).await?;
        Ok(result.deleted_count)
    }

    /// Paginated list of `sample_metadata` documents, optionally filtered by
    /// `target_id` and/or `worth_classifying`.
    ///
    /// Returns `(records_as_json, total_count)`.  Results are sorted by
    /// `analyzed_at` descending (newest first).
    pub async fn fetch_metadata_page(
        &self,
        target_id: Option<&str>,
        worth_classifying: Option<bool>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");

        let mut filter = Document::new();
        if let Some(tid) = target_id {
            filter.insert("target_id", tid);
        }
        if let Some(wc) = worth_classifying {
            filter.insert("ingestion_hints.worth_classifying", wc);
        }

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "analyzed_at": -1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Fetch a single `sample_metadata` document by its `sample_hash`.
    ///
    /// Returns `None` when no document with that hash exists.
    pub async fn fetch_metadata_by_hash(
        &self,
        sample_hash: &str,
    ) -> Result<Option<JsonValue>, AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");
        match col
            .find_one(doc! { "sample_hash": sample_hash }, None)
            .await?
        {
            Some(doc) => Ok(Some(bson_doc_to_json(doc))),
            None => Ok(None),
        }
    }

    /// Return a clone of the destination [`Database`] handle.
    ///
    /// Use this to construct output adapters such as
    /// [`crate::output::graph::GraphWriter`] and
    /// [`crate::output::vector::VectorWriter`], which require direct database
    /// access for their async write operations.
    ///
    /// The handle is cheap to clone — it is backed by an internal `Arc`.
    pub fn destination_db(&self) -> Database {
        self.destination_database.clone()
    }

    fn destination_collection(&self, collection_name: &str) -> Collection<Document> {
        self.destination_database
            .collection::<Document>(collection_name)
    }

    // ─── API methods ──────────────────────────────────────────────────────────

    /// List all target documents regardless of status.
    pub async fn list_all_targets(&self) -> Result<Vec<JsonValue>, AppError> {
        let col = self
            .source_database
            .collection::<Document>(&self.source_collection_name);
        let opts = FindOptions::builder()
            .sort(doc! { "target_id": 1 })
            .build();
        let mut cursor = col.find(doc! {}, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            let doc = cursor.deserialize_current()?;
            out.push(bson_doc_to_json(doc));
        }
        Ok(out)
    }

    /// Insert a new target document.
    pub async fn create_target(&self, body: JsonValue) -> Result<JsonValue, AppError> {
        let col = self
            .source_database
            .collection::<Document>(&self.source_collection_name);
        let mut doc = json_to_bson_doc(body)?;
        // Default status to "active" if not provided.
        if !doc.contains_key("status") {
            doc.insert("status", "active");
        }
        let result = col.insert_one(doc.clone(), None).await?;
        if let Some(id) = result.inserted_id.as_object_id() {
            doc.insert("_id", id);
        }
        Ok(bson_doc_to_json(doc))
    }

    /// Replace a target document identified by its string `_id`.
    pub async fn update_target(&self, id: &str, body: JsonValue) -> Result<JsonValue, AppError> {
        use mongodb::bson::oid::ObjectId;
        let oid = ObjectId::parse_str(id)
            .map_err(|e| AppError::Validation(format!("invalid id: {e}")))?;
        let col = self
            .source_database
            .collection::<Document>(&self.source_collection_name);
        let replacement = json_to_bson_doc(body)?;
        let opts = FindOneAndReplaceOptions::builder()
            .return_document(mongodb::options::ReturnDocument::After)
            .build();
        let updated = col
            .find_one_and_replace(doc! { "_id": oid }, replacement, opts)
            .await?
            .ok_or_else(|| AppError::Validation(format!("target {id} not found")))?;
        Ok(bson_doc_to_json(updated))
    }

    /// Delete a target document by its string `_id`.
    pub async fn delete_target(&self, id: &str) -> Result<(), AppError> {
        use mongodb::bson::oid::ObjectId;
        let oid = ObjectId::parse_str(id)
            .map_err(|e| AppError::Validation(format!("invalid id: {e}")))?;
        let col = self
            .source_database
            .collection::<Document>(&self.source_collection_name);
        col.delete_one(doc! { "_id": oid }, None).await?;
        Ok(())
    }

    /// Delete a sample record (and its metadata) by hash, writing one audit row.
    ///
    /// Removes the document from the per-target collection and from
    /// `sample_metadata`, then inserts one row into `sample_deletions` and
    /// emits a `warn`-level tracing event so the reason is captured in the
    /// service log.
    pub async fn delete_sample(
        &self,
        target_id: &str,
        sample_hash: &str,
        reason: &str,
    ) -> Result<(), AppError> {
        // Remove from per-target collection.
        self.destination_database
            .collection::<Document>(target_id)
            .delete_one(doc! { "sample_hash": sample_hash }, None)
            .await?;

        // Remove associated metadata (best-effort — ignore if absent).
        let _ = self
            .destination_database
            .collection::<Document>("sample_metadata")
            .delete_one(doc! { "sample_hash": sample_hash }, None)
            .await;

        // One audit log row.
        let log_doc = doc! {
            "event":       "sample_deleted",
            "sample_hash": sample_hash,
            "target_id":   target_id,
            "reason":      reason,
            "deleted_at":  DateTime::now(),
        };
        let _ = self
            .destination_database
            .collection::<Document>("sample_deletions")
            .insert_one(log_doc, None)
            .await;

        tracing::warn!(
            sample_hash,
            target_id,
            reason,
            "sample deleted by operator"
        );

        Ok(())
    }

    /// Toggle a target's status between "active" and "inactive".
    pub async fn toggle_target_status(&self, id: &str) -> Result<String, AppError> {
        use mongodb::bson::oid::ObjectId;
        let oid = ObjectId::parse_str(id)
            .map_err(|e| AppError::Validation(format!("invalid id: {e}")))?;
        let col = self
            .source_database
            .collection::<Document>(&self.source_collection_name);

        let current = col
            .find_one(doc! { "_id": oid }, None)
            .await?
            .ok_or_else(|| AppError::Validation(format!("target {id} not found")))?;

        let current_status = current
            .get_str("status")
            .unwrap_or("inactive");
        let new_status = if current_status.eq_ignore_ascii_case("active") {
            "inactive"
        } else {
            "active"
        };

        col.update_one(
            doc! { "_id": oid },
            doc! { "$set": { "status": new_status } },
            None,
        )
        .await?;

        Ok(new_status.to_string())
    }

    /// Paginated list of deletion audit rows from `sample_deletions`.
    pub async fn fetch_deletions_page(
        &self,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_deletions");
        let total = col.count_documents(doc! {}, None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "deleted_at": -1 })
            .skip(skip)
            .limit(limit)
            .build();
        let mut cursor = col.find(doc! {}, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// List all per-target sample collections in the destination database.
    pub async fn list_sample_collections(&self) -> Result<Vec<String>, AppError> {
        let mut names = self
            .destination_database
            .list_collection_names(None)
            .await?;
        // Exclude all known system / auxiliary collections — only actual
        // per-target sample buckets should be visible to the API.
        const SYSTEM_COLLECTIONS: &[&str] = &[
            "sample_metadata",
            "sample_deletions",
            "classifications",
            "app_settings",
            "app_settings_history",
            // UpsideGate output adapter collections.
            "entity_edges",
            "prov_relations",
            "otel_spans",
            "content_embeddings",
            "behavioral_embeddings",
        ];
        names.retain(|n| !SYSTEM_COLLECTIONS.contains(&n.as_str()));
        names.sort();
        Ok(names)
    }

    // ─── UpsideGate output reads ──────────────────────────────────────────────

    /// Fetch a page of relation edges, optionally scoped to a single sample.
    pub async fn fetch_edges_page(
        &self,
        sample_hash: Option<&str>,
        relation_type: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self.destination_database.collection::<Document>("entity_edges");

        let mut filter = Document::new();
        if let Some(h) = sample_hash {
            filter.insert("sample_hash", h);
        }
        if let Some(rt) = relation_type {
            filter.insert("relation_type", rt);
        }

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "created_at": -1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Fetch a page of PROV-O triples, optionally filtered.
    pub async fn fetch_prov_page(
        &self,
        sample_hash: Option<&str>,
        subject: Option<&str>,
        predicate: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self.destination_database.collection::<Document>("prov_relations");

        let mut filter = Document::new();
        if let Some(h) = sample_hash { filter.insert("sample_hash", h); }
        if let Some(s) = subject     { filter.insert("subject", s); }
        if let Some(p) = predicate   { filter.insert("predicate", p); }

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "created_at": -1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Fetch a page of OTel spans, optionally scoped by sample or trace.
    pub async fn fetch_spans_page(
        &self,
        sample_hash: Option<&str>,
        trace_id: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self.destination_database.collection::<Document>("otel_spans");

        let mut filter = Document::new();
        if let Some(h) = sample_hash { filter.insert("sample_hash", h); }
        if let Some(t) = trace_id    { filter.insert("trace_id", t); }

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        // Sort by start time when available, falling back to insertion order.
        let opts = FindOptions::builder()
            .sort(doc! { "start_time_unix_nano": 1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Fetch a paginated page of sample records, optionally filtered by target.
    pub async fn fetch_samples_page(
        &self,
        target_id: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let skip = page * limit as u64;

        if let Some(tid) = target_id {
            // Single collection
            let col = self.destination_database.collection::<Document>(tid);
            let opts = FindOptions::builder()
                .sort(doc! { "timestamp": -1 })
                .skip(skip)
                .limit(limit)
                .build();
            let total = col.count_documents(doc! {}, None).await?;
            let mut cursor = col.find(doc! {}, opts).await?;
            let mut out = Vec::new();
            while cursor.advance().await? {
                out.push(bson_doc_to_json(cursor.deserialize_current()?));
            }
            return Ok((out, total));
        }

        // Across all collections
        let names = self.list_sample_collections().await?;
        let mut all: Vec<JsonValue> = Vec::new();
        let mut total: u64 = 0;

        for name in &names {
            let col = self.destination_database.collection::<Document>(name);
            total += col.count_documents(doc! {}, None).await?;
            let opts = FindOptions::builder()
                .sort(doc! { "timestamp": -1 })
                .limit(limit)
                .build();
            let mut cursor = col.find(doc! {}, opts).await?;
            while cursor.advance().await? {
                all.push(bson_doc_to_json(cursor.deserialize_current()?));
            }
        }

        // Sort combined results by timestamp desc, then paginate
        all.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });
        let page_slice = all
            .into_iter()
            .skip(skip as usize)
            .take(limit as usize)
            .collect();

        Ok((page_slice, total))
    }

    // ─── Classification methods ───────────────────────────────────────────────

    /// Upsert a ClassificationRecord into the `classifications` collection.
    pub async fn store_classification(
        &self,
        record: &ClassificationRecord,
    ) -> Result<(), AppError> {
        self.ensure_classifications_indexes().await?;
        let doc = record.to_document()?;
        let filter = doc! { "sample_hash": &record.sample_hash };
        let opts = ReplaceOptions::builder().upsert(true).build();
        self.destination_database
            .collection::<Document>("classifications")
            .replace_one(filter, doc, opts)
            .await?;
        Ok(())
    }

    /// Set `classification_status` on the matching `sample_metadata` document.
    pub async fn update_classification_status(
        &self,
        hash: &str,
        status: ClassificationStatus,
    ) -> Result<(), AppError> {
        self.destination_database
            .collection::<Document>("sample_metadata")
            .update_one(
                doc! { "sample_hash": hash },
                doc! { "$set": { "classification_status": status.as_str() } },
                None,
            )
            .await?;
        Ok(())
    }

    /// Paginated list of classifications, optionally filtered by target.
    pub async fn fetch_classifications_page(
        &self,
        target_id: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self
            .destination_database
            .collection::<Document>("classifications");

        let filter = if let Some(tid) = target_id {
            doc! { "target_id": tid }
        } else {
            doc! {}
        };

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "classified_at": -1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Fetch samples whose metadata marks them as pending classification.
    ///
    /// Returns pairs of `(SampleRecord, SampleMetadata)` for the caller to
    /// classify.  Only includes samples where `worth_classifying = true`,
    /// `signal_score >= threshold`, and `classification_status = "pending"`.
    pub async fn fetch_pending_classifications(
        &self,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<(SampleRecord, SampleMetadata)>, AppError> {
        let meta_col = self
            .destination_database
            .collection::<Document>("sample_metadata");

        let filter = doc! {
            "ingestion_hints.worth_classifying": true,
            "agentic_scan.signal_score": { "$gte": threshold },
            "classification_status": "pending",
        };
        let opts = FindOptions::builder()
            .sort(doc! { "agentic_scan.signal_score": -1 })
            .limit(limit as i64)
            .build();

        let mut cursor = meta_col.find(filter, opts).await?;
        let mut results = Vec::new();

        while cursor.advance().await? {
            let meta_doc = cursor.deserialize_current()?;

            // Extract target_id and sample_hash to look up the SampleRecord.
            let target_id = match meta_doc.get("target_id") {
                Some(Bson::String(s)) => s.clone(),
                _ => continue,
            };
            let sample_hash = match meta_doc.get("sample_hash") {
                Some(Bson::String(s)) => s.clone(),
                _ => continue,
            };

            // Deserialise the metadata document.
            let metadata = match bson::from_document::<SampleMetadata>(meta_doc) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Look up the SampleRecord in the per-target collection.
            let sample_col = self
                .destination_database
                .collection::<Document>(&target_id);
            let sample_doc = match sample_col
                .find_one(doc! { "sample_hash": &sample_hash }, None)
                .await?
            {
                Some(d) => d,
                None => continue,
            };
            let sample = match bson::from_document::<SampleRecord>(sample_doc) {
                Ok(s) => s,
                Err(_) => continue,
            };

            results.push((sample, metadata));
        }
        Ok(results)
    }

    /// Fetch a single classification by its sample_hash.
    pub async fn find_classification_by_hash(
        &self,
        hash: &str,
    ) -> Result<Option<serde_json::Value>, AppError> {
        let col = self
            .destination_database
            .collection::<Document>("classifications");
        match col.find_one(doc! { "sample_hash": hash }, None).await? {
            Some(doc) => Ok(Some(bson_doc_to_json(doc))),
            None => Ok(None),
        }
    }

    async fn ensure_classifications_indexes(&self) -> Result<(), AppError> {
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains("classifications") {
                return Ok(());
            }
        }
        let col = self
            .destination_database
            .collection::<Document>("classifications");

        let unique_hash = IndexModel::builder()
            .keys(doc! { "sample_hash": 1 })
            .options(
                IndexOptions::builder()
                    .name(Some("unique_classification_hash".to_string()))
                    .unique(Some(true))
                    .build(),
            )
            .build();
        let target_idx = IndexModel::builder()
            .keys(doc! { "target_id": 1 })
            .options(IndexOptions::builder().name(Some("cl_target_id".to_string())).build())
            .build();
        let time_idx = IndexModel::builder()
            .keys(doc! { "classified_at": -1 })
            .options(IndexOptions::builder().name(Some("cl_classified_at".to_string())).build())
            .build();
        let severity_idx = IndexModel::builder()
            .keys(doc! { "severity": 1 })
            .options(IndexOptions::builder().name(Some("cl_severity".to_string())).build())
            .build();

        col.create_index(unique_hash, None).await?;
        col.create_index(target_idx, None).await?;
        col.create_index(time_idx, None).await?;
        col.create_index(severity_idx, None).await?;

        let mut guard = self.indexed_collections.lock().await;
        guard.insert("classifications".to_string());
        Ok(())
    }

    // ─── Admin settings ───────────────────────────────────────────────────────

    /// Load the singleton admin-settings document from `app_settings`.
    pub async fn load_admin_settings(
        &self,
    ) -> Result<Option<crate::config::AdminSettings>, AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        match col.find_one(doc! { "_id": "global" }, None).await? {
            None => Ok(None),
            Some(mut d) => {
                d.remove("_id");
                let settings = bson::from_document::<crate::config::AdminSettings>(d)
                    .map_err(|e| {
                        AppError::Validation(format!(
                            "failed to deserialize admin settings: {e}"
                        ))
                    })?;
                Ok(Some(settings))
            }
        }
    }

    /// Upsert the singleton admin-settings document into `app_settings`.
    /// Sets `_confirmed = true` (used for direct saves that bypass the
    /// canary-confirmation flow, e.g. rollback restores).
    pub async fn save_admin_settings(
        &self,
        settings: &crate::config::AdminSettings,
    ) -> Result<(), AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        let mut doc = bson::to_document(settings)
            .map_err(|e| AppError::Validation(format!("failed to serialize admin settings: {e}")))?;
        doc.insert("_id", "global");
        doc.insert("_confirmed", true);
        doc.remove("_previous");
        let filter = doc! { "_id": "global" };
        let opts = ReplaceOptions::builder().upsert(true).build();
        col.replace_one(filter, doc, opts).await?;
        Ok(())
    }

    /// Upsert admin settings marked as **pending** (awaiting frontend confirmation).
    /// Stores the previously confirmed settings as `_previous` for rollback.
    pub async fn save_admin_settings_pending(
        &self,
        settings: &crate::config::AdminSettings,
        previous: Option<crate::config::AdminSettings>,
    ) -> Result<(), AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        let mut doc = bson::to_document(settings)
            .map_err(|e| AppError::Validation(format!("failed to serialize admin settings: {e}")))?;
        doc.insert("_id", "global");
        doc.insert("_confirmed", false);
        // Store the rollback target
        if let Some(prev) = previous {
            let prev_doc = bson::to_document(&prev).map_err(|e| {
                AppError::Validation(format!("failed to serialize previous admin settings: {e}"))
            })?;
            doc.insert("_previous", prev_doc);
        } else {
            doc.remove("_previous");
        }
        let filter = doc! { "_id": "global" };
        let opts = ReplaceOptions::builder().upsert(true).build();
        col.replace_one(filter, doc, opts).await?;
        Ok(())
    }

    /// Mark the current admin settings as confirmed (cancels any pending rollback).
    pub async fn confirm_admin_settings(&self) -> Result<(), AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        col.update_one(
            doc! { "_id": "global" },
            doc! { "$set": { "_confirmed": true }, "$unset": { "_previous": "" } },
            None,
        )
        .await?;
        Ok(())
    }

    /// Returns `(confirmed, previous_settings)` from the stored metadata.
    /// `confirmed = true` when no rollback is pending.
    pub async fn load_config_pending_meta(
        &self,
    ) -> Result<Option<(bool, Option<crate::config::AdminSettings>)>, AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        let doc = match col.find_one(doc! { "_id": "global" }, None).await? {
            None => return Ok(None),
            Some(d) => d,
        };
        let confirmed = doc.get_bool("_confirmed").unwrap_or(true);
        let previous = if let Ok(prev_doc) = doc.get_document("_previous") {
            bson::from_document::<crate::config::AdminSettings>(prev_doc.clone())
                .ok()
        } else {
            None
        };
        Ok(Some((confirmed, previous)))
    }

    /// Restore the `_previous` snapshot as the active config, mark as confirmed,
    /// and log the rollback event. Returns the restored settings on success.
    pub async fn rollback_admin_settings(
        &self,
        reason: &str,
    ) -> Result<Option<crate::config::AdminSettings>, AppError> {
        let col: mongodb::Collection<Document> =
            self.destination_database.collection("app_settings");
        let doc = match col.find_one(doc! { "_id": "global" }, None).await? {
            None => return Ok(None),
            Some(d) => d,
        };

        let prev_doc = match doc.get_document("_previous") {
            Ok(d) => d.clone(),
            Err(_) => {
                tracing::warn!("rollback requested but no _previous settings stored");
                return Ok(None);
            }
        };

        let previous: crate::config::AdminSettings =
            bson::from_document(prev_doc.clone()).map_err(|e| {
                AppError::Validation(format!(
                    "failed to deserialize previous admin settings for rollback: {e}"
                ))
            })?;

        // Write the previous settings back as the active confirmed config
        self.save_admin_settings(&previous).await?;

        tracing::warn!(reason, "config rolled back to previous confirmed settings");

        // Write a tracking entry to the destination DB so operators can audit
        let rollback_doc = doc! {
            "event":      "config_rollback",
            "reason":     reason,
            "rolled_back_at": DateTime::now(),
        };
        let _ = self
            .destination_database
            .collection::<Document>("app_settings_rollback_log")
            .insert_one(rollback_doc, None)
            .await;

        Ok(Some(previous))
    }

    /// Insert an encrypted, recoverable snapshot of admin settings.
    pub async fn save_admin_settings_history(
        &self,
        settings: &crate::config::AdminSettings,
        config: &ConfigHistoryConfig,
        reason: &str,
    ) -> Result<i64, AppError> {
        self.ensure_config_history_indexes(&config.collection_name).await?;
        let col = self
            .destination_database
            .collection::<Document>(&config.collection_name);

        let version = self.next_config_history_version(&config.collection_name).await?;
        let document = config_history::build_history_document(settings, config, version, reason)?;
        col.insert_one(document, None).await?;
        Ok(version)
    }

    async fn next_config_history_version(&self, collection_name: &str) -> Result<i64, AppError> {
        let col = self.destination_database.collection::<Document>(collection_name);
        let opts = FindOneOptions::builder()
            .sort(doc! { "version": -1 })
            .projection(doc! { "version": 1, "_id": 0 })
            .build();

        let Some(document) = col.find_one(doc! {}, opts).await? else {
            return Ok(1);
        };

        Ok(match document.get("version") {
            Some(Bson::Int64(value)) => value + 1,
            Some(Bson::Int32(value)) => i64::from(*value) + 1,
            _ => 1,
        })
    }

    async fn ensure_config_history_indexes(
        &self,
        collection_name: &str,
    ) -> Result<(), AppError> {
        let index_key = format!("config_history:{collection_name}");
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains(&index_key) {
                return Ok(());
            }
        }

        let col = self.destination_database.collection::<Document>(collection_name);
        let version_idx = IndexModel::builder()
            .keys(doc! { "version": -1 })
            .options(
                IndexOptions::builder()
                    .name(Some("config_history_version".to_string()))
                    .unique(Some(true))
                    .build(),
            )
            .build();
        let created_at_idx = IndexModel::builder()
            .keys(doc! { "created_at": -1 })
            .options(
                IndexOptions::builder()
                    .name(Some("config_history_created_at".to_string()))
                    .build(),
            )
            .build();

        col.create_index(version_idx, None).await?;
        col.create_index(created_at_idx, None).await?;

        let mut guard = self.indexed_collections.lock().await;
        guard.insert(index_key);
        Ok(())
    }

    /// Return a lightweight list of config-history metadata entries (newest first).
    /// Each entry contains version, created_at, created_by, source, reason, and
    /// the encryption key_id — but NOT the settings payload or wrapped keys.
    pub async fn list_config_history(
        &self,
        collection_name: &str,
        limit: i64,
    ) -> Result<Vec<Document>, AppError> {
        let col = self
            .destination_database
            .collection::<Document>(collection_name);

        let opts = FindOptions::builder()
            .sort(doc! { "version": -1 })
            .limit(limit)
            .projection(doc! {
                "version":    1,
                "created_at": 1,
                "created_by": 1,
                "source":     1,
                "reason":     1,
                "encryption": 1,
                "_id":        0,
            })
            .build();

        let mut cursor = col.find(doc! {}, opts).await?;
        let mut results = Vec::new();
        while cursor.advance().await? {
            results.push(cursor.deserialize_current()?);
        }
        Ok(results)
    }

    /// Fetch a single history version, decrypt its secret fields using
    /// `master_key`, and return the reconstituted `AdminSettings`.
    pub async fn restore_config_history_entry(
        &self,
        collection_name: &str,
        version: i64,
        master_key: &str,
    ) -> Result<crate::config::AdminSettings, AppError> {
        let col = self
            .destination_database
            .collection::<Document>(collection_name);

        let document = col
            .find_one(doc! { "version": version }, None)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!("config history version {version} not found"))
            })?;

        let mut settings_doc = document
            .get_document("settings")
            .map_err(|e| AppError::Validation(format!("missing settings in history doc: {e}")))?
            .clone();

        // Decrypt each secret field that was stored as an EncryptedSecret.
        use crate::config_history::{decrypt_secret, EncryptedSecret};
        for field in crate::config_history::SECRET_FIELDS {
            if let Ok(inner) = settings_doc.get_document(field) {
                // Only attempt decrypt if encrypted == true
                if inner.get_bool("encrypted").unwrap_or(false) {
                    let envelope: EncryptedSecret =
                        bson::from_document(inner.clone()).map_err(|e| {
                            AppError::Validation(format!(
                                "failed to deserialize EncryptedSecret for {field}: {e}"
                            ))
                        })?;
                    let plaintext = decrypt_secret(&envelope, master_key)?;
                    settings_doc.insert(*field, plaintext);
                }
            }
        }

        let settings: crate::config::AdminSettings =
            bson::from_document(settings_doc).map_err(|e| {
                AppError::Validation(format!(
                    "failed to deserialize restored admin settings: {e}"
                ))
            })?;

        Ok(settings)
    }

    /// Fetch paginated records from `loggingtracker.logging_tracks`.
    pub async fn fetch_tracking_logs(
        &self,
        limit: i64,
        page: u64,
        search: Option<&str>,
        level: Option<&str>,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self
            .tracking_database
            .collection::<Document>(&self.tracking_collection_name);

        let mut filter = doc! {};
        if let Some(lvl) = level {
            if !lvl.is_empty() {
                filter.insert("level", lvl);
            }
        }
        if let Some(q) = search {
            if !q.is_empty() {
                filter.insert(
                    "message",
                    doc! { "$regex": q, "$options": "i" },
                );
            }
        }

        let total = col.count_documents(filter.clone(), None).await?;
        let skip = page * limit as u64;
        let opts = FindOptions::builder()
            .sort(doc! { "timestamp": -1 })
            .skip(skip)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }

        Ok((out, total))
    }

    // ─── Entity lookup ────────────────────────────────────────────────────────

    /// Fetch a single [`EntityRecord`] by its `entity_id`.
    ///
    /// Entities are not stored in a collection of their own — they live in the
    /// `entities` array of the owning `sample_metadata` document.  This uses a
    /// positional projection (`entities.$`) so Mongo returns only the matching
    /// array element instead of shipping every entity in the sample across the
    /// wire.
    ///
    /// Accepts either a bare `entity_id` or the PROV URI form
    /// `ug:entity:{entity_id}`, since the frontend holds URIs in PROV views and
    /// bare ids in entity views.
    ///
    /// Returns `None` when no sample contains that entity.
    ///
    /// [`EntityRecord`]: crate::models::EntityRecord
    pub async fn fetch_entity_by_id(
        &self,
        entity_id: &str,
    ) -> Result<Option<JsonValue>, AppError> {
        let entity_id = strip_entity_uri(entity_id);
        self.ensure_entity_id_index().await;

        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");

        let opts = FindOneOptions::builder()
            .projection(doc! { "entities.$": 1, "sample_hash": 1 })
            .build();

        let Some(doc) = col
            .find_one(doc! { "entities.entity_id": entity_id }, opts)
            .await?
        else {
            return Ok(None);
        };

        // The positional projection should return exactly the matching element,
        // but that guarantee is the server's, not ours — and it silently lapses
        // if the filter is ever changed in a way Mongo cannot project
        // positionally.  Confirm the id before trusting it, so a lapse becomes a
        // 404 rather than a confidently wrong entity.
        let entity = doc
            .get_array("entities")
            .ok()
            .and_then(|arr| arr.first())
            .and_then(Bson::as_document)
            .filter(|d| d.get_str("entity_id") == Ok(entity_id))
            .cloned();

        Ok(entity.map(bson_doc_to_json))
    }

    /// Fetch many [`EntityRecord`]s by id in one round trip.
    ///
    /// Used to hydrate traversal results: BFS yields entity *ids*, but the UI
    /// needs labels and types, so the ids are resolved in a single aggregation
    /// rather than N positional-projection queries.
    ///
    /// Ids that do not resolve are silently absent from the result — a
    /// traversal can legitimately reach an edge endpoint whose entity document
    /// was deleted, and a missing label should not fail the whole request.
    ///
    /// [`EntityRecord`]: crate::models::EntityRecord
    pub async fn fetch_entities_by_ids(
        &self,
        entity_ids: &[String],
    ) -> Result<Vec<JsonValue>, AppError> {
        if entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_entity_id_index().await;

        let ids: Vec<&str> = entity_ids.iter().map(String::as_str).collect();
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");

        // $match narrows to candidate samples on the index, $unwind explodes the
        // arrays, and the second $match keeps only the entities we asked for.
        let pipeline = vec![
            doc! { "$match":       { "entities.entity_id": { "$in": &ids } } },
            doc! { "$unwind":      "$entities" },
            doc! { "$match":       { "entities.entity_id": { "$in": &ids } } },
            doc! { "$replaceRoot": { "newRoot": "$entities" } },
        ];

        let mut cursor = col.aggregate(pipeline, None).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok(out)
    }

    // ─── Graph traversal ──────────────────────────────────────────────────────

    /// Breadth-first walk of `entity_edges` from `root`, following `direction`.
    ///
    /// Issues one indexed query per depth level (matching the frontier with
    /// `$in`) rather than one per node, so a wide graph costs the same number of
    /// round trips as a narrow one.
    ///
    /// Returns the full edge documents in discovery order, the entity records
    /// for every visited node, and whether the [`MAX_NODES`] budget truncated
    /// the walk.
    ///
    /// [`MAX_NODES`]: super::graph_query::MAX_NODES
    pub async fn traverse_graph(
        &self,
        root: &str,
        direction: Direction,
        depth: u32,
        include_structural: bool,
    ) -> Result<TraversalResult, AppError> {
        let walk = self
            .walk_edges(root, direction, depth, include_structural)
            .await?;
        // Mixed hydration: since Stage 12 an edge endpoint may be an actor rather
        // than an event, and both have to resolve or the graph renders unlabelled
        // nodes for every agent, skill and resource it reaches.
        let entities = self.fetch_graph_nodes_by_ids(&walk.node_ids).await?;

        // Surface nodes that did not resolve to an entity record rather than
        // letting them vanish. With structural edges included this is expected
        // (the trace pseudo-node); without them it means a dangling edge, and
        // silently dropping it would leave the caller wondering why the graph
        // has fewer labels than nodes.
        // Events are keyed on `entity_id`, actors on `actor_id`. Checking only the
        // former would report every actor node as unresolved even though it
        // hydrated perfectly well.
        let resolved: HashSet<&str> = entities
            .iter()
            .filter_map(|e| {
                e.get("entity_id")
                    .or_else(|| e.get("actor_id"))
                    .and_then(JsonValue::as_str)
            })
            .collect();
        let unresolved_node_ids: Vec<String> = walk
            .node_ids
            .iter()
            .filter(|id| !resolved.contains(id.as_str()))
            .cloned()
            .collect();

        Ok(TraversalResult {
            root: walk.root,
            direction,
            depth_reached: walk.depth_reached,
            edges: walk.edges,
            entities,
            node_ids: walk.node_ids,
            unresolved_node_ids,
            truncated: walk.truncated,
        })
    }

    /// BFS over `entity_edges` without hydrating entity records.
    ///
    /// Split out of [`traverse_graph`](Self::traverse_graph) because
    /// [`graph_path`](Self::graph_path) needs the edges but not the
    /// neighbourhood's entities — hydrating up to [`MAX_NODES`] records only to
    /// discard them costs a large `$in` aggregation per path request.
    ///
    /// [`MAX_NODES`]: super::graph_query::MAX_NODES
    async fn walk_edges(
        &self,
        root: &str,
        direction: Direction,
        depth: u32,
        include_structural: bool,
    ) -> Result<EdgeWalk, AppError> {
        let root = strip_entity_uri(root);
        let col = self
            .destination_database
            .collection::<Document>("entity_edges");

        let mut traversal = Traversal::new(root, direction, depth);
        // relation_id → full edge document, so edges can be returned in the
        // order the traversal discovered them.
        let mut edge_docs: HashMap<String, JsonValue> = HashMap::new();

        while !traversal.is_done() {
            let frontier: Vec<&str> = traversal.frontier().iter().map(String::as_str).collect();
            let mut filter = doc! { direction.match_field(): { "$in": &frontier } };
            if !include_structural {
                // See STRUCTURAL_RELATION_TYPES: these point at the trace, not
                // at an entity, so following them adds an unlabelled dead end to
                // every walk.
                filter.insert(
                    "relation_type",
                    doc! { "$nin": STRUCTURAL_RELATION_TYPES },
                );
            }

            // Cap the per-level fetch. Without this a single hub entity with a
            // huge fan-out would be fully materialised as JSON before the
            // traversal's own budget ever got a chance to stop it.
            let remaining = MAX_EDGES.saturating_sub(edge_docs.len());
            if remaining == 0 {
                traversal.mark_truncated();
                break;
            }
            let opts = FindOptions::builder()
                .limit(remaining as i64)
                .build();

            let mut cursor = col.find(filter, opts).await?;
            let mut level = Vec::new();
            while cursor.advance().await? {
                let doc = cursor.deserialize_current()?;
                let Some(endpoints) = edge_endpoints(&doc) else {
                    // An edge missing an endpoint field cannot be traversed;
                    // skip it rather than aborting the walk.
                    continue;
                };
                edge_docs.insert(endpoints.relation_id.clone(), bson_doc_to_json(doc));
                level.push(endpoints);
            }
            // A full page means Mongo had more to give: the graph is wider than
            // the budget, so the result is partial.
            if level.len() >= remaining {
                traversal.mark_truncated();
            }
            traversal.absorb_level(&level);
        }

        let edges: Vec<JsonValue> = traversal
            .edge_ids()
            .iter()
            .filter_map(|id| edge_docs.get(id).cloned())
            .collect();

        Ok(EdgeWalk {
            root: root.to_string(),
            depth_reached: traversal.depth_reached(),
            truncated: traversal.truncated(),
            edges,
            node_ids: traversal.into_visited(),
        })
    }

    /// Shortest directed path between two entities, or `None` if unreachable.
    ///
    /// Loads the edges reachable from `from` within `max_depth` hops, then runs
    /// BFS over them in memory.  Pulling the neighbourhood first keeps this to
    /// `max_depth` queries; running the search server-side would need
    /// `$graphLookup`, which cannot report *which* edge it used for each hop —
    /// and the edge identity is the point of a provenance path.
    pub async fn graph_path(
        &self,
        from: &str,
        to: &str,
        max_depth: u32,
    ) -> Result<PathOutcome, AppError> {
        let from = strip_entity_uri(from);
        let to = strip_entity_uri(to);

        if from.is_empty() || to.is_empty() {
            return Err(AppError::Validation(
                "graph path requires non-empty `from` and `to`".to_string(),
            ));
        }

        // Answer the degenerate case before paying for a traversal.
        if from == to {
            return Ok(PathOutcome::Found(PathResult {
                from: from.to_string(),
                to: to.to_string(),
                hops: Vec::new(),
                edges: Vec::new(),
                entities: self.fetch_entities_by_ids(&[from.to_string()]).await?,
                node_ids: vec![from.to_string()],
                truncated: false,
            }));
        }

        // Collect the reachable neighbourhood's edges. Entity records are not
        // hydrated here — only the nodes on the winning path need them.
        // Structural edges are excluded: the trace pseudo-node is a sink, so it
        // can never lie on a path, and fetching those edges is pure waste.
        let reachable = self
            .walk_edges(from, Direction::Downstream, max_depth, false)
            .await?;

        let endpoints: Vec<EdgeEndpoints> = reachable
            .edges
            .iter()
            .filter_map(json_edge_endpoints)
            .collect();

        let Some(hops) = shortest_path(&endpoints, from, to, max_depth) else {
            // A truncated search cannot distinguish "no path" from "gave up",
            // so say which one happened rather than asserting there is no path.
            return Ok(if reachable.truncated {
                PathOutcome::Truncated
            } else {
                PathOutcome::NotFound
            });
        };

        // Return the edge documents for the chosen hops, in path order.
        let hop_ids: HashSet<&str> = hops.iter().map(|h| h.relation_id.as_str()).collect();
        let mut edges_by_id: HashMap<&str, &JsonValue> = HashMap::new();
        for edge in &reachable.edges {
            if let Some(id) = edge.get("relation_id").and_then(JsonValue::as_str) {
                if hop_ids.contains(id) {
                    edges_by_id.insert(id, edge);
                }
            }
        }
        let edges: Vec<JsonValue> = hops
            .iter()
            .filter_map(|h| edges_by_id.get(h.relation_id.as_str()).map(|e| (*e).clone()))
            .collect();

        // Nodes on the path only — not the whole neighbourhood we searched.
        let mut node_ids = vec![from.to_string()];
        node_ids.extend(hops.iter().map(|h| h.to.clone()));
        let entities = self.fetch_entities_by_ids(&node_ids).await?;

        Ok(PathOutcome::Found(PathResult {
            from: from.to_string(),
            to: to.to_string(),
            hops,
            edges,
            entities,
            node_ids,
            truncated: reachable.truncated,
        }))
    }

    // ─── Actors (Stage 12) ────────────────────────────────────────────────────

    /// Upsert an actor, accumulating across samples.
    ///
    /// Actors are cross-sample by design — the model `claude-opus-4` is one node
    /// everywhere — so like tasks this accumulates rather than replaces:
    /// `$addToSet` unions the samples and tasks it took part in, `$inc` advances
    /// the reference count, `$max` widens the time window.
    ///
    /// `event_delta` should be `0` when re-processing a sample already recorded
    /// against this actor, for the same reason as [`Self::upsert_task`].
    pub async fn upsert_actor(
        &self,
        actor: &ActorRecord,
        event_delta: u32,
    ) -> Result<(), AppError> {
        self.ensure_actor_indexes().await;

        let col = self.destination_database.collection::<Document>(ACTORS_COLL);
        let now = DateTime::now();

        let mut add_to_set = doc! {};
        if let Some(hash) = actor.sample_hashes.first() {
            add_to_set.insert("sample_hashes", hash);
        }
        if let Some(task) = actor.task_ids.first() {
            add_to_set.insert("task_ids", task);
        }

        let mut update = doc! {
            "$setOnInsert": {
                "actor_id":     &actor.actor_id,
                "kind":         actor.kind.as_str(),
                "name":         &actor.name,
                "source_field": &actor.source_field,
                "first_seen":   now,
            },
            "$inc": { "event_count": event_delta as i64 },
            "$max": { "last_seen": now },
        };
        if !add_to_set.is_empty() {
            update.insert("$addToSet", add_to_set);
        }

        col.update_one(
            doc! { "actor_id": &actor.actor_id },
            update,
            UpdateOptions::builder().upsert(true).build(),
        )
        .await?;

        Ok(())
    }

    /// Whether an actor has already been counted for a given sample.
    ///
    /// Guards the `$inc` in [`Self::upsert_actor`] against re-processing, exactly
    /// as [`Self::task_sample_counted`] does for tasks.
    pub async fn actor_sample_counted(
        &self,
        actor_id: &str,
        sample_hash: &str,
    ) -> Result<bool, AppError> {
        let col = self.destination_database.collection::<Document>(ACTORS_COLL);
        Ok(col
            .find_one(
                doc! { "actor_id": actor_id, "sample_hashes": sample_hash },
                FindOneOptions::builder().projection(doc! { "_id": 1 }).build(),
            )
            .await?
            .is_some())
    }

    /// Fetch actors by id.
    ///
    /// Used to hydrate actor nodes reached by a graph traversal — see
    /// [`Self::fetch_graph_nodes_by_ids`].
    pub async fn fetch_actors_by_ids(
        &self,
        actor_ids: &[String],
    ) -> Result<Vec<JsonValue>, AppError> {
        if actor_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<&str> = actor_ids.iter().map(String::as_str).collect();
        let col = self.destination_database.collection::<Document>(ACTORS_COLL);

        let mut cursor = col
            .find(doc! { "actor_id": { "$in": &ids } }, None)
            .await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok(out)
    }

    /// Resolve graph node ids to their records, whichever kind they are.
    ///
    /// A traversal walks `entity_edges` without knowing whether a given endpoint
    /// is an event or an actor — since Stage 12, edges point at both. This looks
    /// in `sample_metadata.entities` first and then in `actors` for whatever did
    /// not resolve, so a mixed graph hydrates completely rather than reporting
    /// actor nodes as unresolved.
    pub async fn fetch_graph_nodes_by_ids(
        &self,
        node_ids: &[String],
    ) -> Result<Vec<JsonValue>, AppError> {
        let mut nodes = self.fetch_entities_by_ids(node_ids).await?;

        let resolved: HashSet<&str> = nodes
            .iter()
            .filter_map(|e| e.get("entity_id").and_then(JsonValue::as_str))
            .collect();
        let missing: Vec<String> = node_ids
            .iter()
            .filter(|id| !resolved.contains(id.as_str()))
            .cloned()
            .collect();

        if !missing.is_empty() {
            nodes.extend(self.fetch_actors_by_ids(&missing).await?);
        }
        Ok(nodes)
    }

    /// Page through actors, most recently active first.
    pub async fn fetch_actors_page(
        &self,
        kind: Option<&str>,
        task_id: Option<&str>,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self.destination_database.collection::<Document>(ACTORS_COLL);

        let mut filter = Document::new();
        if let Some(k) = kind {
            filter.insert("kind", k);
        }
        if let Some(t) = task_id {
            filter.insert("task_ids", t);
        }

        let total = col.count_documents(filter.clone(), None).await?;
        let opts = FindOptions::builder()
            .sort(doc! { "last_seen": -1 })
            .skip(page * limit as u64)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Indexes for the `actors` collection. Best-effort, like the others.
    async fn ensure_actor_indexes(&self) {
        const MARKER: &str = "actors::indexes";
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains(MARKER) {
                return;
            }
        }

        let col = self.destination_database.collection::<Document>(ACTORS_COLL);
        let models = vec![
            IndexModel::builder()
                .keys(doc! { "actor_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some("actors_unique_actor_id".to_string()))
                        .unique(Some(true))
                        .build(),
                )
                .build(),
            // "which agents worked on this task"
            IndexModel::builder()
                .keys(doc! { "task_ids": 1 })
                .options(IndexOptions::builder().name(Some("actors_task_ids".to_string())).build())
                .build(),
            IndexModel::builder()
                .keys(doc! { "kind": 1, "last_seen": -1 })
                .options(IndexOptions::builder().name(Some("actors_kind_last_seen".to_string())).build())
                .build(),
        ];

        for model in models {
            if let Err(e) = col.create_index(model, None).await {
                warn!(error = %e, "could not create an actors index; queries will scan");
            }
        }

        self.indexed_collections
            .lock()
            .await
            .insert(MARKER.to_string());
    }

    // ─── Tasks (Stage 11) ─────────────────────────────────────────────────────

    /// Upsert a task, accumulating rather than replacing.
    ///
    /// A task can span several samples, and those samples arrive independently and
    /// in any order — so this **must not** be a `replace_one`. `$addToSet` unions
    /// the sample hashes, trace ids and target ids; `$inc` accumulates the counts;
    /// `$min` / `$max` widen the observed time window; and `$setOnInsert` fixes the
    /// immutable identity fields on first write only.
    ///
    /// Idempotent: re-processing the same sample adds nothing to the sets, and the
    /// counts are `$inc`'d by the *delta* the caller computes rather than blindly,
    /// so a re-run does not inflate them. See [`Self::task_sample_counted`].
    pub async fn upsert_task(
        &self,
        task_id: &str,
        task_id_source: &str,
        correlation_key: Option<&str>,
        sample_hash: &str,
        trace_id: &str,
        target_id: &str,
        entity_delta: u32,
        relation_delta: u32,
    ) -> Result<(), AppError> {
        self.ensure_task_indexes().await;

        let col = self.destination_database.collection::<Document>(TASKS_COLL);
        let now = DateTime::now();

        let mut set_on_insert = doc! {
            "task_id":        task_id,
            "task_id_source": task_id_source,
            "first_seen":     now,
        };
        if let Some(key) = correlation_key {
            set_on_insert.insert("correlation_key", key);
        }

        let update = doc! {
            "$setOnInsert": set_on_insert,
            "$addToSet": {
                "sample_hashes": sample_hash,
                "trace_ids":     trace_id,
                "target_ids":    target_id,
            },
            "$inc": {
                "entity_count":   entity_delta as i64,
                "relation_count": relation_delta as i64,
            },
            "$max": { "last_seen": now },
        };

        col.update_one(
            doc! { "task_id": task_id },
            update,
            UpdateOptions::builder().upsert(true).build(),
        )
        .await?;

        Ok(())
    }

    /// Record a task's intent, but only if it does not already have one.
    ///
    /// Returns `true` when this call actually set it — the caller uses that to
    /// decide whether an embedding needs generating, so re-processing does not
    /// re-bill the embedding API.
    ///
    /// **First writer wins**, deliberately. A task's goal is stated once, usually
    /// in its first sample; a later sample of the same task carries continuation
    /// text, and letting that overwrite would replace the goal with whatever the
    /// agent happened to be saying by then. The filter on `intent_text: null` makes
    /// this atomic rather than a read-then-write race between concurrent samples.
    pub async fn set_task_intent_if_absent(
        &self,
        task_id: &str,
        intent_text: &str,
    ) -> Result<bool, AppError> {
        let col = self.destination_database.collection::<Document>(TASKS_COLL);
        let result = col
            .update_one(
                doc! {
                    "task_id": task_id,
                    "$or": [
                        { "intent_text": { "$exists": false } },
                        { "intent_text": null },
                    ],
                },
                doc! { "$set": { "intent_text": intent_text } },
                None,
            )
            .await?;
        Ok(result.modified_count > 0)
    }

    /// Whether a sample has already been counted into its task.
    ///
    /// Callers check this before [`Self::upsert_task`] so the `$inc` counters are
    /// only advanced the first time a given sample joins a task. Without it,
    /// re-processing a sample would double-count its entities and relations —
    /// `$addToSet` is idempotent but `$inc` is not.
    pub async fn task_sample_counted(
        &self,
        task_id: &str,
        sample_hash: &str,
    ) -> Result<bool, AppError> {
        let col = self.destination_database.collection::<Document>(TASKS_COLL);
        let found = col
            .find_one(
                doc! { "task_id": task_id, "sample_hashes": sample_hash },
                FindOneOptions::builder().projection(doc! { "_id": 1 }).build(),
            )
            .await?;
        Ok(found.is_some())
    }

    /// Fetch one task by id.
    pub async fn fetch_task(&self, task_id: &str) -> Result<Option<JsonValue>, AppError> {
        let col = self.destination_database.collection::<Document>(TASKS_COLL);
        Ok(col
            .find_one(doc! { "task_id": task_id }, None)
            .await?
            .map(bson_doc_to_json))
    }

    /// Page through tasks, newest activity first.
    ///
    /// `real_boundaries_only` excludes tasks whose id came from the sample
    /// fallback — useful because those are not task boundaries in any meaningful
    /// sense, just one-sample-per-task placeholders for logs with no correlation
    /// key.
    pub async fn fetch_tasks_page(
        &self,
        target_id: Option<&str>,
        real_boundaries_only: bool,
        limit: i64,
        page: u64,
    ) -> Result<(Vec<JsonValue>, u64), AppError> {
        let col = self.destination_database.collection::<Document>(TASKS_COLL);

        let mut filter = Document::new();
        if let Some(tid) = target_id {
            filter.insert("target_ids", tid);
        }
        if real_boundaries_only {
            filter.insert("task_id_source", doc! { "$ne": SAMPLE_FALLBACK });
        }

        let total = col.count_documents(filter.clone(), None).await?;
        let opts = FindOptions::builder()
            .sort(doc! { "last_seen": -1 })
            .skip(page * limit as u64)
            .limit(limit)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut out = Vec::new();
        while cursor.advance().await? {
            out.push(bson_doc_to_json(cursor.deserialize_current()?));
        }
        Ok((out, total))
    }

    /// Indexes for the `tasks` collection.
    ///
    /// Best-effort like [`Self::ensure_entity_id_index`], since tasks are read on
    /// API paths where a read-only Mongo user should still be able to query.
    async fn ensure_task_indexes(&self) {
        const MARKER: &str = "tasks::indexes";
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains(MARKER) {
                return;
            }
        }

        let col = self.destination_database.collection::<Document>(TASKS_COLL);
        let models = vec![
            IndexModel::builder()
                .keys(doc! { "task_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name(Some("tasks_unique_task_id".to_string()))
                        .unique(Some(true))
                        .build(),
                )
                .build(),
            // The join key back to the graph: "which task does this sample belong to".
            IndexModel::builder()
                .keys(doc! { "sample_hashes": 1 })
                .options(IndexOptions::builder().name(Some("tasks_sample_hashes".to_string())).build())
                .build(),
            IndexModel::builder()
                .keys(doc! { "last_seen": -1 })
                .options(IndexOptions::builder().name(Some("tasks_last_seen".to_string())).build())
                .build(),
        ];

        for model in models {
            if let Err(e) = col.create_index(model, None).await {
                warn!(error = %e, "could not create a tasks index; queries will scan");
            }
        }

        self.indexed_collections
            .lock()
            .await
            .insert(MARKER.to_string());
    }

    // ─── Similarity search ────────────────────────────────────────────────────

    /// Fetch a sample's own embedding vector, for search-by-example.
    ///
    /// Returns `None` when that sample has no embedding of the requested kind —
    /// common for `Content`, which needs an API key and is off by default.
    pub async fn fetch_embedding_vector(
        &self,
        sample_hash: &str,
        kind: EmbeddingKind,
    ) -> Result<Option<Vec<f32>>, AppError> {
        let col = self
            .destination_database
            .collection::<Document>(embedding_collection(kind));

        let opts = FindOneOptions::builder()
            .projection(doc! { "vector": 1 })
            .build();

        // Task embeddings are keyed on `task_id`; the sample-scoped kinds on
        // `sample_hash`. Looking up the wrong field would silently find nothing
        // and read as "this task has no embedding".
        let Some(doc) = col
            .find_one(doc! { key_field(kind): sample_hash }, opts)
            .await?
        else {
            return Ok(None);
        };

        Ok(doc.get_array("vector").ok().map(|arr| {
            arr.iter()
                // Vectors are written as f32 but BSON has no f32 — the driver
                // stores them as doubles, so read as f64 and narrow back.
                .filter_map(|b| b.as_f64().map(|f| f as f32))
                .collect()
        }))
    }

    /// Rank stored embeddings of `kind` against `query` by cosine similarity.
    ///
    /// Scans and scores in process rather than using `$vectorSearch`: that
    /// operator needs a vector index only MongoDB Atlas provides, and a
    /// self-hosted standalone `mongod` — what the dev stack runs — cannot serve
    /// it. Embeddings are one record per sample, so the collection is small
    /// relative to the raw data; [`MAX_SCAN`] bounds it anyway.
    ///
    /// `exclude_sample_hash` drops the query's own sample, which otherwise always
    /// ranks first at `1.0` and crowds out the answer the caller wanted.
    ///
    /// [`MAX_SCAN`]: super::vector_query::MAX_SCAN
    pub async fn search_embeddings(
        &self,
        kind: EmbeddingKind,
        query: &[f32],
        limit: usize,
        target_id: Option<&str>,
        exclude_sample_hash: Option<&str>,
    ) -> Result<SearchResult, AppError> {
        let col = self
            .destination_database
            .collection::<Document>(embedding_collection(kind));

        let mut filter = Document::new();
        if let Some(hash) = exclude_sample_hash {
            // Excluding "self" means a different field per kind — see `key_field`.
            filter.insert(key_field(kind), doc! { "$ne": hash });
        }
        // Embedding records carry no target_id, so narrowing by target means
        // resolving that target's sample hashes first.
        if let Some(tid) = target_id {
            let hashes = self.sample_hashes_for_target(tid).await?;
            if hashes.is_empty() {
                return Ok(SearchResult {
                    hits: Vec::new(),
                    scored: 0,
                    skipped: 0,
                    truncated: false,
                });
            }
            filter.insert("sample_hash", doc! { "$in": hashes });
        }

        let opts = FindOptions::builder()
            .projection(
                doc! { "sample_hash": 1, "task_id": 1, "embedding_id": 1, "model": 1, "vector": 1 },
            )
            .limit(MAX_SCAN as i64)
            .build();

        let mut cursor = col.find(filter, opts).await?;
        let mut candidates: Vec<Candidate> = Vec::new();
        while cursor.advance().await? {
            let doc = cursor.deserialize_current()?;
            let Ok(vector_arr) = doc.get_array("vector") else {
                continue;
            };
            candidates.push(Candidate {
                sample_hash: doc.get_str("sample_hash").unwrap_or_default().to_string(),
                task_id: doc.get_str("task_id").ok().map(str::to_string),
                embedding_id: doc.get_str("embedding_id").unwrap_or_default().to_string(),
                model: doc.get_str("model").unwrap_or_default().to_string(),
                vector: vector_arr
                    .iter()
                    .filter_map(|b| b.as_f64().map(|f| f as f32))
                    .collect(),
            });
        }

        // A full page means the collection is larger than the scan budget, so
        // the ranking is over a subset.
        let truncated = candidates.len() >= MAX_SCAN;
        let ranking = rank(query, &candidates, limit);

        Ok(SearchResult {
            hits: ranking.hits,
            scored: ranking.scored,
            skipped: ranking.skipped,
            truncated,
        })
    }

    /// Sample hashes belonging to `target_id`, for filtering embeddings.
    async fn sample_hashes_for_target(&self, target_id: &str) -> Result<Vec<String>, AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");
        let opts = FindOptions::builder()
            .projection(doc! { "sample_hash": 1, "_id": 0 })
            .build();

        let mut cursor = col.find(doc! { "target_id": target_id }, opts).await?;
        let mut hashes = Vec::new();
        while cursor.advance().await? {
            if let Ok(hash) = cursor.deserialize_current()?.get_str("sample_hash") {
                hashes.push(hash.to_string());
            }
        }
        Ok(hashes)
    }

    /// Create the multikey index backing `entities.entity_id` lookups.
    ///
    /// Without it, every entity lookup is a full collection scan of
    /// `sample_metadata`.  Tracked in the shared `indexed_collections` set so
    /// the round trip happens once per process, matching the pattern used by
    /// the other `ensure_*` helpers.
    ///
    /// Unlike those helpers this one runs on a **read** path, so a failure must
    /// not fail the request: an operator running the API with read-only Mongo
    /// credentials should still be able to query, just without the index.  The
    /// error is logged and the marker set either way, so a rejected attempt is
    /// not retried on every subsequent request.
    async fn ensure_entity_id_index(&self) {
        const MARKER: &str = "sample_metadata::entities.entity_id";
        {
            let guard = self.indexed_collections.lock().await;
            if guard.contains(MARKER) {
                return;
            }
        }

        let result = self
            .destination_database
            .collection::<Document>("sample_metadata")
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "entities.entity_id": 1 })
                    .options(
                        IndexOptions::builder()
                            .name(Some("sm_entities_entity_id".to_string()))
                            .build(),
                    )
                    .build(),
                None,
            )
            .await;

        if let Err(e) = result {
            warn!(
                error = %e,
                "could not create sm_entities_entity_id index; entity lookups \
                 will fall back to a collection scan",
            );
        }

        self.indexed_collections
            .lock()
            .await
            .insert(MARKER.to_string());
    }
}

// ─── Traversal result types ───────────────────────────────────────────────────

/// Result of [`MongoRepository::traverse_graph`].
///
/// `edges` + `entities` are shaped to drop straight into the frontend's
/// `RelationGraph` component, which takes exactly those two arrays.
#[derive(Debug, Serialize)]
pub struct TraversalResult {
    pub root: String,
    pub direction: Direction,
    pub depth_reached: u32,
    pub edges: Vec<JsonValue>,
    pub entities: Vec<JsonValue>,
    pub node_ids: Vec<String>,
    /// Visited ids with no corresponding entity record. Normally empty; non-empty
    /// means either a structural edge was followed or an edge is dangling.
    pub unresolved_node_ids: Vec<String>,
    pub truncated: bool,
}

/// Edges and nodes from a BFS walk, before entity hydration.
struct EdgeWalk {
    root: String,
    depth_reached: u32,
    truncated: bool,
    edges: Vec<JsonValue>,
    node_ids: Vec<String>,
}

/// Outcome of [`MongoRepository::graph_path`].
///
/// Three states rather than `Option`, because "searched exhaustively and found
/// nothing" and "ran out of budget before finishing" are different answers and
/// the caller should be able to tell the user which one it got.
#[derive(Debug)]
pub enum PathOutcome {
    Found(PathResult),
    /// Search completed; the target is genuinely unreachable within the depth.
    NotFound,
    /// Search hit a budget, so no conclusion about reachability is possible.
    Truncated,
}

/// Result of [`MongoRepository::search_embeddings`].
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub hits: Vec<ScoredHit>,
    /// Candidates that had a comparable vector.
    pub scored: usize,
    /// Candidates skipped as incomparable — `scored == 0` with `skipped > 0`
    /// means the query's dimensionality did not match the collection's.
    pub skipped: usize,
    /// True when the collection exceeded the scan budget, so the ranking covers
    /// only a subset.
    pub truncated: bool,
}

/// Which collection holds embeddings of a given kind.
fn embedding_collection(kind: EmbeddingKind) -> &'static str {
    match kind {
        EmbeddingKind::Content => CONTENT_EMBEDDINGS_COLL,
        EmbeddingKind::Behavioral => BEHAVIORAL_EMBEDDINGS_COLL,
        EmbeddingKind::Task => TASK_EMBEDDINGS_COLL,
    }
}

/// Which field identifies an embedding record of a given kind.
///
/// Content and behavioral vectors are per sample; a task's intent vector is per
/// task, because a task spans samples. Getting this wrong does not error — it
/// finds nothing, and reads as "no embedding exists".
fn key_field(kind: EmbeddingKind) -> &'static str {
    match kind {
        EmbeddingKind::Task => "task_id",
        _ => "sample_hash",
    }
}

/// A resolved path between two entities.
#[derive(Debug, Serialize)]
pub struct PathResult {
    pub from: String,
    pub to: String,
    pub hops: Vec<PathHop>,
    pub edges: Vec<JsonValue>,
    pub entities: Vec<JsonValue>,
    pub node_ids: Vec<String>,
    /// True when the neighbourhood search was truncated.  The returned path is
    /// still valid, but a shorter one could in principle have been missed.
    pub truncated: bool,
}

// ─── Edge helpers ─────────────────────────────────────────────────────────────

/// Strip the `ug:entity:` PROV URI prefix, if present.
///
/// PROV views hold `ug:entity:{id}` while entity views hold the bare id; both
/// reach these endpoints, so normalise on the way in.
fn strip_entity_uri(id: &str) -> &str {
    id.strip_prefix("ug:entity:").unwrap_or(id)
}

/// Pull the traversable endpoints out of a raw `entity_edges` BSON document.
fn edge_endpoints(doc: &Document) -> Option<EdgeEndpoints> {
    Some(EdgeEndpoints {
        relation_id: doc.get_str("relation_id").ok()?.to_string(),
        source_entity_id: doc.get_str("source_entity_id").ok()?.to_string(),
        target_entity_id: doc.get_str("target_entity_id").ok()?.to_string(),
    })
}

/// Same as [`edge_endpoints`] but for an already-converted JSON edge.
fn json_edge_endpoints(edge: &JsonValue) -> Option<EdgeEndpoints> {
    Some(EdgeEndpoints {
        relation_id: edge.get("relation_id")?.as_str()?.to_string(),
        source_entity_id: edge.get("source_entity_id")?.as_str()?.to_string(),
        target_entity_id: edge.get("target_entity_id")?.as_str()?.to_string(),
    })
}

// ─── BSON ↔ JSON helpers ──────────────────────────────────────────────────────

fn bson_doc_to_json(doc: Document) -> JsonValue {
    // Convert ObjectId to string so the frontend can use it as a plain id field.
    let mut map = serde_json::Map::new();
    for (k, v) in doc {
        let key = if k == "_id" { "id".to_string() } else { k };
        map.insert(key, bson_to_json(v));
    }
    JsonValue::Object(map)
}

fn bson_to_json(v: Bson) -> JsonValue {
    match v {
        Bson::ObjectId(oid) => JsonValue::String(oid.to_hex()),
        Bson::String(s) => JsonValue::String(s),
        Bson::Boolean(b) => JsonValue::Bool(b),
        Bson::Int32(i) => JsonValue::Number(i.into()),
        Bson::Int64(i) => JsonValue::Number(i.into()),
        Bson::Double(d) => serde_json::Number::from_f64(d)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Bson::DateTime(dt) => JsonValue::String(dt.to_rfc3339_string()),
        Bson::Array(arr) => JsonValue::Array(arr.into_iter().map(bson_to_json).collect()),
        Bson::Document(doc) => bson_doc_to_json(doc),
        Bson::Null => JsonValue::Null,
        other => JsonValue::String(other.to_string()),
    }
}

fn json_to_bson_doc(v: JsonValue) -> Result<Document, AppError> {
    bson::to_document(&v)
        .map_err(|e| AppError::Validation(format!("failed to convert JSON to BSON: {e}")))
}

fn is_duplicate_key_error(error: &MongoError) -> bool {
    error.to_string().contains("E11000") || error.to_string().contains("duplicate key")
}
