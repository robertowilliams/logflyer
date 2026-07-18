//! Integration tests for logflayer's MongoDB persistence layer.
//!
//! Each test spins up a real MongoDB instance via `testcontainers` and
//! exercises the actual [`MongoRepository`] methods so we catch any
//! mismatch between our BSON serialisation and the driver's expectations.
//!
//! Two repository methods gate the downstream classifier:
//!
//! - `fetch_unprocessed_samples` — returns `SampleRecord`s that have no
//!   corresponding `SampleMetadata` yet (used by backfill to decide what still
//!   needs preprocessing).
//! - `fetch_pending_classifications` — returns `(SampleRecord, SampleMetadata)`
//!   pairs where `worth_classifying = true`, `signal_score >= threshold`, and
//!   `classification_status = "pending"` (used by the classifier worker).
//!
//! Run with:
//!   cargo test --test integration_test
//!
//! Docker must be available on the host (used by testcontainers to launch
//! the MongoDB container).

use std::path::Path;

use logflayer::config::{MongoConfig, PreprocessingConfig};
use logflayer::models::{ClassificationStatus, ProcessingStatus, SampleRecord};
use logflayer::preprocessing::Preprocessor;
use logflayer::repository::{MongoRepository, StoreOutcome};
use logflayer::sampling::SamplingMode;
use mongodb::bson::DateTime;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn make_config(host: &str, port: u16) -> MongoConfig {
    MongoConfig {
        uri: format!("mongodb://{}:{}", host, port),
        source_db_name: "vectadb_test".to_string(),
        source_collection_name: "ai_targets".to_string(),
        destination_db_name: "log_samples_test".to_string(),
        tracking_db_name: "tracking_test".to_string(),
        tracking_collection_name: "logging_tracks".to_string(),
    }
}

fn default_preprocessing_config() -> PreprocessingConfig {
    PreprocessingConfig {
        enabled: true,
        agentic_threshold: 0.02,
        max_schema_lines: 200,
        metrics_port: 0, // disable metrics HTTP listener in tests
    }
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
}

fn make_sample(target_id: &str, source_file: &str, content: &str, hash: &str) -> SampleRecord {
    SampleRecord {
        timestamp: DateTime::now(),
        sample_hash: hash.to_string(),
        target_id: target_id.to_string(),
        source_file: source_file.to_string(),
        sample_content: content.to_string(),
        host: "test-host".to_string(),
        path: source_file.to_string(),
        sampling_mode: SamplingMode::Both,
        line_count: Some(content.lines().count() as u64),
        file_size_bytes: Some(content.len() as u64),
        processing_status: ProcessingStatus::Stored,
        error_details: None,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Inserting the same sample hash twice must return `Duplicate` the second
/// time — the unique index on `sample_hash` is the deduplication mechanism.
#[tokio::test]
async fn test_store_sample_deduplication() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let sample = make_sample("tgt-dd", "/var/log/app.log", "hello world\n", "hash-dd-001");

    assert_eq!(
        repo.store_sample("tgt-dd", &sample).await.unwrap(),
        StoreOutcome::Inserted,
        "first store must insert"
    );
    assert_eq!(
        repo.store_sample("tgt-dd", &sample).await.unwrap(),
        StoreOutcome::Duplicate,
        "same hash must be rejected as duplicate"
    );
}

/// `store_metadata` is an upsert — calling it twice with the same hash must
/// not fail on a duplicate key error.
#[tokio::test]
async fn test_store_metadata_upsert_idempotent() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let content = fixture("langchain_json.log");
    let metadata = Preprocessor::new(default_preprocessing_config())
        .run("hash-up-001", "tgt-up", &content);

    repo.store_metadata(&metadata).await.expect("first upsert");
    repo.store_metadata(&metadata).await.expect("second upsert must not error");
}

/// Full lifecycle for an agentic (LangChain JSON) log:
///
/// 1. Before preprocessing: sample appears in `fetch_unprocessed_samples`.
/// 2. After preprocessing + metadata stored: sample disappears from
///    `fetch_unprocessed_samples` and appears in `fetch_pending_classifications`.
#[tokio::test]
async fn test_langchain_sample_lifecycle() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let content = fixture("langchain_json.log");
    let hash = "hash-lc-001";
    let target = "tgt-lc";
    let sample = make_sample(target, "/app/langchain.log", &content, hash);

    // Step 1: store the raw sample
    assert_eq!(
        repo.store_sample(target, &sample).await.unwrap(),
        StoreOutcome::Inserted
    );

    // Before metadata exists the sample should appear as unprocessed
    let unprocessed_before = repo.fetch_unprocessed_samples(10).await.unwrap();
    assert!(
        unprocessed_before.iter().any(|s| s.sample_hash == hash),
        "sample must appear in fetch_unprocessed_samples before metadata is stored"
    );

    // Step 2: run preprocessing and store metadata
    let metadata = Preprocessor::new(default_preprocessing_config()).run(hash, target, &content);

    assert!(
        metadata.agentic_scan.worth_classifying,
        "LangChain log must be worth classifying (score={})",
        metadata.agentic_scan.signal_score
    );
    assert!(metadata.schema.is_some(), "JSON log must produce a schema");
    assert_eq!(metadata.classification_status, ClassificationStatus::Pending);

    repo.store_metadata(&metadata).await.unwrap();

    // After metadata is stored the sample must no longer appear as unprocessed
    let unprocessed_after = repo.fetch_unprocessed_samples(10).await.unwrap();
    assert!(
        !unprocessed_after.iter().any(|s| s.sample_hash == hash),
        "sample must NOT appear in fetch_unprocessed_samples after metadata is stored"
    );

    // And it must surface in fetch_pending_classifications (worth_classifying=true)
    let pending = repo.fetch_pending_classifications(0.0, 10).await.unwrap();
    assert!(
        pending.iter().any(|(s, _)| s.sample_hash == hash),
        "agentic sample must appear in fetch_pending_classifications"
    );
}

/// Samples whose preprocessing result is `worth_classifying = false` must NOT
/// appear in `fetch_pending_classifications` — the classifier must never waste
/// LLM tokens on non-agentic logs such as plain nginx access logs.
#[tokio::test]
async fn test_skip_logic_not_worth_classifying() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let content = fixture("nginx_access.log");
    let hash = "hash-ng-001";
    let target = "tgt-ng";
    let sample = make_sample(target, "/var/log/nginx/access.log", &content, hash);

    repo.store_sample(target, &sample).await.unwrap();

    let metadata = Preprocessor::new(default_preprocessing_config()).run(hash, target, &content);

    assert!(
        !metadata.agentic_scan.worth_classifying || metadata.agentic_scan.signal_score < 0.02,
        "nginx log must NOT be worth classifying (score={})",
        metadata.agentic_scan.signal_score
    );

    repo.store_metadata(&metadata).await.unwrap();

    // Must not appear in the classifier queue
    let pending = repo.fetch_pending_classifications(0.0, 10).await.unwrap();
    assert!(
        !pending.iter().any(|(s, _)| s.sample_hash == hash),
        "nginx sample must NOT appear in fetch_pending_classifications"
    );

    // Must not appear as unprocessed either (metadata exists)
    let unprocessed = repo.fetch_unprocessed_samples(10).await.unwrap();
    assert!(
        !unprocessed.iter().any(|s| s.sample_hash == hash),
        "nginx sample must NOT appear in fetch_unprocessed_samples after metadata is stored"
    );
}

/// Two samples with different hashes under the same target collection must
/// be stored independently — neither shadows the other.
#[tokio::test]
async fn test_two_samples_independent_storage() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let a = make_sample("tgt-x", "/log/a.log", "line one\n", "hash-x-001");
    let b = make_sample("tgt-x", "/log/b.log", "line two\n", "hash-x-002");

    assert_eq!(
        repo.store_sample("tgt-x", &a).await.unwrap(),
        StoreOutcome::Inserted,
        "sample A must insert"
    );
    assert_eq!(
        repo.store_sample("tgt-x", &b).await.unwrap(),
        StoreOutcome::Inserted,
        "sample B must insert independently"
    );

    let unprocessed = repo.fetch_unprocessed_samples(10).await.unwrap();
    assert!(
        unprocessed.iter().any(|s| s.sample_hash == "hash-x-001"),
        "sample A must be unprocessed"
    );
    assert!(
        unprocessed.iter().any(|s| s.sample_hash == "hash-x-002"),
        "sample B must be unprocessed"
    );
}
