use std::collections::HashSet;
use std::sync::Arc;

use mongodb::bson::{self, doc, Bson, Document};
use mongodb::error::Error as MongoError;
use mongodb::options::{ClientOptions, FindOptions, IndexOptions, ReplaceOptions};
use mongodb::{Client, Collection, Database, IndexModel};
use tokio::sync::Mutex;

use crate::config::MongoConfig;
use crate::error::AppError;
use crate::models::{SampleMetadata, SampleRecord};

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
    source_collection_name: String,
    indexed_collections: Arc<Mutex<HashSet<String>>>,
}

impl MongoRepository {
    pub async fn connect(config: &MongoConfig) -> Result<Self, AppError> {
        let mut options = ClientOptions::parse(&config.uri).await?;
        options.app_name = Some("logflayer".to_string());

        let client = Client::with_options(options)?;
        let source_database = client.database(&config.source_db_name);
        let destination_database = client.database(&config.destination_db_name);

        Ok(Self {
            client,
            source_database,
            destination_database,
            source_collection_name: config.source_collection_name.clone(),
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
        // Each target writes into its own collection so data can be isolated, queried,
        // or retained independently per remote system.
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
        // Deduplication is enforced by MongoDB itself through a unique hash index. That
        // keeps repeated polling idempotent even when multiple service instances run.
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

    /// Upsert a [`SampleMetadata`] document into the shared `sample_metadata`
    /// collection, keyed by `sample_hash`.
    ///
    /// Using `replace_one` with `upsert: true` means a re-run of the
    /// preprocessor on the same sample will overwrite the old metadata rather
    /// than accumulating duplicates.
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

    /// Fetch up to `limit` sample records whose preprocessing has not yet been
    /// completed.
    ///
    /// "Unprocessed" is defined as having no corresponding document in the
    /// `sample_metadata` collection.  This method performs the anti-join in
    /// application code rather than with a `$lookup` to keep the query simple
    /// and compatible with all MongoDB deployments.
    ///
    /// For large backlogs a future iteration should push this into a server-side
    /// aggregation, but for the initial implementation the fetch-then-filter
    /// approach is adequate.
    pub async fn fetch_unprocessed_samples(
        &self,
        limit: usize,
    ) -> Result<Vec<SampleRecord>, AppError> {
        // Collect the hashes that already have metadata.
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

        // Scan each per-target collection in the destination database and
        // collect unprocessed records.
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

                // Reconstruct a minimal SampleRecord from the stored document.
                if let Ok(record) = bson::from_document::<SampleRecord>(document) {
                    unprocessed.push(record);
                }
            }
        }

        Ok(unprocessed)
    }

    async fn ensure_metadata_indexes(&self) -> Result<(), AppError> {
        // Guard: only create the indexes once per process lifetime.
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

    /// Delete all `SampleMetadata` documents whose `preprocessing_version` does
    /// not match `current_version`.  The backfill loop will subsequently
    /// re-process those samples because they will no longer appear in
    /// `fetch_unprocessed_samples`'s anti-join exclusion set.
    ///
    /// Returns the number of documents deleted.
    pub async fn delete_stale_metadata(&self, current_version: &str) -> Result<u64, AppError> {
        let col = self
            .destination_database
            .collection::<Document>("sample_metadata");

        let filter = doc! {
            "preprocessing_version": { "$ne": current_version }
        };

        let result = col.delete_many(filter, None).await?;
        Ok(result.deleted_count)
    }

    fn destination_collection(&self, collection_name: &str) -> Collection<Document> {
        self.destination_database
            .collection::<Document>(collection_name)
    }
}

fn is_duplicate_key_error(error: &MongoError) -> bool {
    error.to_string().contains("E11000") || error.to_string().contains("duplicate key")
}
