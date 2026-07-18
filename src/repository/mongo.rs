use std::collections::HashSet;
use std::sync::Arc;

use mongodb::bson::{self, doc, Bson, DateTime, Document};
use mongodb::error::Error as MongoError;
use mongodb::options::{
    ClientOptions, FindOneAndReplaceOptions, FindOneOptions, FindOptions, IndexOptions,
    ReplaceOptions,
};
use mongodb::{Client, Collection, Database, IndexModel};
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;

use crate::config::{ConfigHistoryConfig, MongoConfig};
use crate::config_history;
use crate::error::AppError;
use crate::models::{ClassificationRecord, ClassificationStatus, SampleMetadata, SampleRecord};

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
