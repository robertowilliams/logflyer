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
use logflayer::models::{ClassificationStatus, ProcessingStatus, SampleMetadata, SampleRecord};
use logflayer::config::EmbeddingConfig;
use logflayer::embedding::{EmbeddingKind, EmbeddingWorker};
use logflayer::output::graph::GraphWriter;
use logflayer::output::vector::VectorWriter;
use logflayer::preprocessing::Preprocessor;
use logflayer::repository::{Direction, MongoRepository, PathOutcome, StoreOutcome};
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
        // Stages 6–10 on, so these tests exercise entity extraction, relation
        // linking and the graph/vector outputs rather than only stages 1–5.
        entity_extraction_enabled: true,
        // 0 disables the persistence gate: write outputs for any sample with
        // at least one entity, so fixtures do not need to be large.
        min_entities_for_persist: 0,
        task_correlation_enabled: true,
        actor_nodes_enabled: true,
    }
}

/// Run the pipeline and return just the [`SampleMetadata`].
///
/// `Preprocessor::run` returns a [`PipelineOutput`] bundling metadata with the
/// PROV triples and OTel spans; most tests here only care about the metadata.
fn preprocess(hash: &str, target: &str, content: &str) -> SampleMetadata {
    Preprocessor::new(default_preprocessing_config())
        .run(hash, target, content)
        .metadata
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
    let metadata = preprocess("hash-up-001", "tgt-up", &content);

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
    let metadata = preprocess(hash, target, &content);

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

    let metadata = preprocess(hash, target, &content);

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

// ─── Graph traversal ──────────────────────────────────────────────────────────
//
// The traversal algorithms are unit-tested in-memory in
// `repository::graph_query`, so these tests deliberately cover only what those
// cannot: the actual MongoDB interaction — the positional projection used to
// pull one entity out of a `sample_metadata.entities` array, the aggregation
// that hydrates many, and the per-level `$in` queries against `entity_edges`.

/// Pick an edge that actually connects two entities.
///
/// `relations[0]` is a `PART_OF` edge — `emit_part_of` runs first — whose target
/// is the sample's trace id rather than an entity. Traversal excludes those by
/// default, so a test that grabs the first relation ends up asserting things
/// about the trace pseudo-node instead of real lineage.
/// Note that `mcp_session.log` is **not** usable here: it yields 19 McpEvent
/// entities and 19 `PART_OF` edges and nothing else, because no relation rule
/// pairs an MCP request with its response. Use a fixture that produces real
/// lineage.
fn first_entity_to_entity_edge(metadata: &SampleMetadata) -> &logflayer::models::RelationEdge {
    metadata
        .relations
        .iter()
        .find(|r| format!("{:?}", r.relation_type) != "PartOf")
        .expect("fixture must produce at least one entity-to-entity relation")
}

/// Seed a sample's metadata and its behavioral embedding; return the sample hash.
///
/// Content embeddings are deliberately not seeded — they need an API call, which
/// is exactly the condition the search endpoint has to handle gracefully.
async fn seed_embeddings(
    repo: &MongoRepository,
    hash: &str,
    target: &str,
    fixture_name: &str,
) -> String {
    let content = fixture(fixture_name);
    let metadata = preprocess(hash, target, &content);
    repo.store_metadata(&metadata).await.expect("store metadata");

    // Behavioral vectors are computed locally, so no embedding API is involved.
    let bundle = EmbeddingWorker::new(EmbeddingConfig {
        enabled: false,
        api_key: String::new(),
        api_base_url: String::new(),
        model: "text-embedding-3-small".to_string(),
        max_text_chars: 8_000,
        dimensions: 1536,
    })
    .expect("build embedding worker")
    // `enabled: false` means the content vector is skipped without an API call;
    // the behavioral vector is computed locally and always returned.
    .embed_sample(
        hash,
        &content,
        &metadata.entities,
        &metadata.relations,
        metadata.agentic_scan.signal_score,
    )
    .await;

    VectorWriter::new(repo.destination_db())
        .write(&bundle.into_records("text-embedding-3-small"))
        .await
        .expect("write embeddings");

    hash.to_string()
}

/// Seed a sample's metadata plus its edges, and return the stored metadata.
///
/// Edges must go through `GraphWriter` rather than being written directly, so
/// the tests exercise the same serialisation the live pipeline uses.
async fn seed_graph(repo: &MongoRepository, hash: &str, target: &str, fixture_name: &str) -> SampleMetadata {
    let content = fixture(fixture_name);
    let out = Preprocessor::new(default_preprocessing_config()).run(hash, target, &content);
    repo.store_metadata(&out.metadata).await.expect("store metadata");

    GraphWriter::new(repo.destination_db())
        .write_edges(&out.metadata.relations)
        .await
        .expect("write edges");

    // Actor edges travel with `metadata.relations`, so the actor *records* have to
    // be written too — otherwise traversal reaches an id with nothing behind it
    // and correctly reports a dangling node. The service does both together.
    for actor in &out.actors {
        repo.upsert_actor(actor, actor.event_count)
            .await
            .expect("upsert actor");
    }

    out.metadata
}

/// A bare id and its `ug:entity:` URI form must resolve to the same record,
/// and the positional projection must return the *requested* entity rather
/// than simply the first element of the array.
#[tokio::test]
async fn test_fetch_entity_by_id_accepts_both_id_forms() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let metadata = seed_graph(&repo, "hash-ent-001", "tgt-ent", "langchain_json.log").await;
    assert!(
        metadata.entities.len() > 1,
        "fixture must yield several entities so the projection is actually discriminating"
    );

    // Deliberately not entities[0] — a broken projection would return the first
    // element and this test would still pass if we asked for it.
    let wanted = metadata.entities.last().expect("at least one entity");

    let by_id = repo
        .fetch_entity_by_id(&wanted.entity_id)
        .await
        .expect("lookup by bare id")
        .expect("entity must be found");
    assert_eq!(
        by_id.get("entity_id").and_then(|v| v.as_str()),
        Some(wanted.entity_id.as_str()),
        "must return the requested entity, not just the first in the array"
    );

    let by_uri = repo
        .fetch_entity_by_id(&format!("ug:entity:{}", wanted.entity_id))
        .await
        .expect("lookup by PROV URI")
        .expect("URI form must resolve too");
    assert_eq!(by_id, by_uri, "both id spellings must resolve identically");

    assert!(
        repo.fetch_entity_by_id("nonexistent-entity-id").await.unwrap().is_none(),
        "unknown id must be None, not an error"
    );
}

/// The hydration aggregation must return exactly the requested entities —
/// no extras from the same sample, and missing ids silently omitted.
#[tokio::test]
async fn test_fetch_entities_by_ids_returns_only_requested() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let metadata = seed_graph(&repo, "hash-ent-002", "tgt-ent2", "langchain_json.log").await;
    assert!(metadata.entities.len() >= 2, "need at least two entities");

    let wanted: Vec<String> = metadata
        .entities
        .iter()
        .take(2)
        .map(|e| e.entity_id.clone())
        .collect();

    let mut ids = wanted.clone();
    ids.push("definitely-not-a-real-entity".to_string());

    let fetched = repo.fetch_entities_by_ids(&ids).await.expect("hydrate");
    assert_eq!(
        fetched.len(),
        2,
        "must return the two real entities and silently skip the missing one"
    );

    let returned: Vec<&str> = fetched
        .iter()
        .filter_map(|e| e.get("entity_id").and_then(|v| v.as_str()))
        .collect();
    for id in &wanted {
        assert!(returned.contains(&id.as_str()), "missing requested entity {id}");
    }

    assert!(
        repo.fetch_entities_by_ids(&[]).await.unwrap().is_empty(),
        "empty input must short-circuit to an empty result"
    );
}

/// Walking downstream from the first entity must reach further than depth 1,
/// and walking upstream from a downstream node must lead back to it.
#[tokio::test]
async fn test_traverse_graph_follows_edges_in_both_directions() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let metadata = seed_graph(&repo, "hash-trv-001", "tgt-trv", "openai_chat_completions.log").await;
    let first_edge = first_entity_to_entity_edge(&metadata);
    let root = first_edge.source_entity_id.clone();
    let neighbour = first_edge.target_entity_id.clone();

    let down = repo
        .traverse_graph(&root, Direction::Downstream, 3, false)
        .await
        .expect("downstream traversal");

    assert_eq!(down.root, root);
    assert!(
        down.node_ids.contains(&neighbour),
        "depth-3 downstream walk must reach the direct neighbour"
    );
    assert!(!down.edges.is_empty(), "must return the edges it walked");
    assert!(
        down.entities.iter().any(|e| {
            e.get("entity_id").and_then(|v| v.as_str()) == Some(root.as_str())
        }),
        "traversal must hydrate entity records for visited nodes"
    );
    assert!(!down.truncated, "a fixture-sized graph must not hit any budget");

    let up = repo
        .traverse_graph(&neighbour, Direction::Upstream, 3, false)
        .await
        .expect("upstream traversal");
    assert!(
        up.node_ids.contains(&root),
        "walking upstream from the neighbour must reach the root again"
    );

    // An entity with no edges at all still yields itself and nothing more.
    let isolated = repo
        .traverse_graph("no-such-entity", Direction::Downstream, 3, false)
        .await
        .expect("traversal of an unknown root must not error");
    assert_eq!(isolated.node_ids, vec!["no-such-entity".to_string()]);
    assert!(isolated.edges.is_empty());
    assert_eq!(isolated.depth_reached, 0);
}

/// `PART_OF` edges point at the sample's OTel trace id, not at an entity.
///
/// Every entity has one, so if traversal followed them by default every walk
/// would pick up the same unlabelled dead-end node. This pins the exclusion —
/// and pins that opting in still works and reports the unresolvable node rather
/// than dropping it silently.
#[tokio::test]
async fn test_structural_part_of_edges_are_excluded_by_default() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let metadata = seed_graph(&repo, "hash-str-001", "tgt-str", "openai_chat_completions.log").await;

    // Confirm the fixture actually exercises the case.
    let part_of: Vec<_> = metadata
        .relations
        .iter()
        .filter(|r| format!("{:?}", r.relation_type) == "PartOf")
        .collect();
    assert!(
        !part_of.is_empty(),
        "fixture must produce PART_OF edges for this test to mean anything"
    );
    let trace_id = &metadata.otel_trace_id;
    assert!(
        part_of.iter().all(|r| &r.target_entity_id == trace_id),
        "PART_OF targets should be the sample's trace id"
    );

    let root = &metadata.entities.first().expect("need an entity").entity_id;

    // Default: the trace pseudo-node must not appear at all.
    let excluded = repo
        .traverse_graph(root, Direction::Downstream, 3, false)
        .await
        .expect("traversal excluding structural edges");
    assert!(
        !excluded.node_ids.contains(trace_id),
        "trace pseudo-node must not be visited by default, got {:?}",
        excluded.node_ids,
    );
    assert!(
        excluded.unresolved_node_ids.is_empty(),
        "every visited node should hydrate when structural edges are excluded, unresolved: {:?}",
        excluded.unresolved_node_ids,
    );

    // Opted in: the node appears, and is reported as unresolvable rather than
    // silently missing from `entities`.
    let included = repo
        .traverse_graph(root, Direction::Downstream, 3, true)
        .await
        .expect("traversal including structural edges");
    assert!(
        included.node_ids.contains(trace_id),
        "trace pseudo-node must be visited when explicitly requested"
    );
    assert!(
        included.unresolved_node_ids.contains(trace_id),
        "the trace has no entity record, so it must be reported as unresolved"
    );
    assert!(
        included.node_ids.len() > excluded.node_ids.len(),
        "including structural edges must widen the walk"
    );
}

/// `graph_path` must find a real path, distinguish an unreachable pair from a
/// truncated search, and reject empty input.
#[tokio::test]
async fn test_graph_path_resolves_and_reports_absence() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let metadata = seed_graph(&repo, "hash-pth-001", "tgt-pth", "openai_chat_completions.log").await;
    let edge = first_entity_to_entity_edge(&metadata);

    match repo
        .graph_path(&edge.source_entity_id, &edge.target_entity_id, 6)
        .await
        .expect("path lookup")
    {
        PathOutcome::Found(path) => {
            assert_eq!(path.hops.len(), 1, "directly connected pair is one hop");
            assert_eq!(path.hops[0].relation_id, edge.relation_id);
            assert_eq!(path.edges.len(), 1, "one edge document per hop");
            assert!(!path.truncated);
        }
        other => panic!("expected a path between two directly-connected entities, got {other:?}"),
    }

    // Reversed: edges are directed and the walk only follows them outward, so
    // unless the fixture happens to contain an explicit back edge there is no
    // path the other way. Assert that concretely rather than accepting any
    // outcome — a test that admits both answers cannot fail.
    let has_back_edge = metadata.relations.iter().any(|r| {
        r.source_entity_id == edge.target_entity_id
            && r.target_entity_id == edge.source_entity_id
    });
    let reversed = repo
        .graph_path(&edge.target_entity_id, &edge.source_entity_id, 6)
        .await
        .expect("reverse path lookup");
    if has_back_edge {
        assert!(
            matches!(reversed, PathOutcome::Found(_)),
            "fixture contains an explicit back edge, so the reverse path must resolve"
        );
    } else {
        assert!(
            matches!(reversed, PathOutcome::NotFound),
            "no back edge exists, so the reverse lookup must be a definite NotFound \
             (Truncated would mean the budget tripped on a fixture-sized graph)"
        );
    }

    // Same entity: zero-length path, and no traversal needed.
    match repo
        .graph_path(&edge.source_entity_id, &edge.source_entity_id, 6)
        .await
        .expect("self path")
    {
        PathOutcome::Found(path) => {
            assert!(path.hops.is_empty(), "path to self has no hops");
            assert_eq!(path.node_ids.len(), 1);
        }
        other => panic!("path to self must be Found, got {other:?}"),
    }

    assert!(
        repo.graph_path("", &edge.target_entity_id, 6).await.is_err(),
        "empty `from` is a caller error, not an empty result"
    );
}

// ─── Tasks (Stage 11) ─────────────────────────────────────────────────────────

/// Fold a sample into its task the way the service does, and return the correlation.
async fn fold_into_task(
    repo: &MongoRepository,
    hash: &str,
    target: &str,
    fixture_name: &str,
) -> logflayer::preprocessing::task_correlator::TaskCorrelation {
    let content = fixture(fixture_name);
    let out = Preprocessor::new(default_preprocessing_config()).run(hash, target, &content);
    let correlation = out.task_correlation.clone().expect("correlation enabled in tests");
    repo.store_metadata(&out.metadata).await.expect("store metadata");

    let counted = repo
        .task_sample_counted(&correlation.task_id, hash)
        .await
        .expect("membership check");
    let (ed, rd) = if counted {
        (0, 0)
    } else {
        (out.metadata.entity_count, out.metadata.relation_count)
    };

    repo.upsert_task(
        &correlation.task_id,
        &correlation.source,
        correlation.correlation_key.as_deref(),
        hash,
        &out.metadata.otel_trace_id,
        target,
        ed,
        rd,
    )
    .await
    .expect("upsert task");

    correlation
}

/// The property the whole phase exists for: two samples sharing a correlation key
/// must accumulate into **one** task document, in either arrival order.
#[tokio::test]
async fn test_task_accumulates_across_samples() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    // Same fixture, two different sample hashes — so the same session id.
    let a = fold_into_task(&repo, "hash-task-a", "tgt-task", "langchain_json.log").await;
    let b = fold_into_task(&repo, "hash-task-b", "tgt-task", "langchain_json.log").await;

    assert_eq!(a.task_id, b.task_id, "shared correlation key must be one task");
    assert!(a.is_real_boundary(), "langchain carries a real key");

    let task = repo
        .fetch_task(&a.task_id)
        .await
        .expect("fetch")
        .expect("task must exist");

    let hashes: Vec<&str> = task["sample_hashes"]
        .as_array()
        .expect("sample_hashes array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(hashes.len(), 2, "both samples must be recorded, got {hashes:?}");
    assert!(hashes.contains(&"hash-task-a"));
    assert!(hashes.contains(&"hash-task-b"));

    // Two samples, two distinct sample-scoped traces, both retained.
    assert_eq!(
        task["trace_ids"].as_array().expect("trace_ids").len(),
        2,
        "trace_id stays sample-scoped, so a two-sample task has two traces",
    );
    // One target, added twice — $addToSet must not duplicate it.
    assert_eq!(task["target_ids"].as_array().expect("target_ids").len(), 1);

    assert!(task["entity_count"].as_i64().unwrap_or(0) > 0);
    assert_eq!(task["task_id_source"].as_str(), Some("run_id"));
    assert!(task["correlation_key"].as_str().is_some());
}

/// Re-processing a sample must not inflate the task's counters.
///
/// `$addToSet` is idempotent but `$inc` is not, which is why the service checks
/// membership first. This pins that guard.
#[tokio::test]
async fn test_reprocessing_a_sample_does_not_double_count() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let first = fold_into_task(&repo, "hash-dup", "tgt-dup", "langchain_json.log").await;
    let after_first = repo.fetch_task(&first.task_id).await.unwrap().unwrap();
    let entities_after_first = after_first["entity_count"].as_i64().unwrap();

    // Same sample again — the exact case a re-run or a backfill produces.
    fold_into_task(&repo, "hash-dup", "tgt-dup", "langchain_json.log").await;
    let after_second = repo.fetch_task(&first.task_id).await.unwrap().unwrap();

    assert_eq!(
        after_second["entity_count"].as_i64().unwrap(),
        entities_after_first,
        "re-processing must not advance the counters",
    );
    assert_eq!(
        after_second["sample_hashes"].as_array().unwrap().len(),
        1,
        "the sample must appear once, not twice",
    );
}

/// Samples with no correlation key must stay in separate tasks, and be labelled
/// as fallbacks so an audit can tell.
#[tokio::test]
async fn test_samples_without_a_key_do_not_merge() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    // Raw JSON-RPC has no session concept at all.
    let a = fold_into_task(&repo, "hash-mcp-a", "tgt-mcp", "mcp_session.log").await;
    let b = fold_into_task(&repo, "hash-mcp-b", "tgt-mcp", "mcp_session.log").await;

    assert_ne!(a.task_id, b.task_id, "no key means no merging");
    assert!(!a.is_real_boundary());
    assert_eq!(a.source, "sample");

    // Both exist as separate one-sample tasks.
    for correlation in [&a, &b] {
        let task = repo.fetch_task(&correlation.task_id).await.unwrap().unwrap();
        assert_eq!(task["sample_hashes"].as_array().unwrap().len(), 1);
        assert_eq!(task["task_id_source"].as_str(), Some("sample"));
        assert!(task["correlation_key"].is_null(), "no key to record");
    }
}

/// `real_boundaries_only` must exclude sample-fallback tasks, since those are not
/// task boundaries in any meaningful sense.
#[tokio::test]
async fn test_fetch_tasks_page_can_exclude_fallbacks() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let real = fold_into_task(&repo, "hash-mix-a", "tgt-mix", "langchain_json.log").await;
    let fallback = fold_into_task(&repo, "hash-mix-b", "tgt-mix", "mcp_session.log").await;

    let (all, all_total) = repo.fetch_tasks_page(None, false, 50, 0).await.unwrap();
    assert_eq!(all_total, 2, "both tasks are listed by default");
    assert_eq!(all.len(), 2);

    let (only_real, real_total) = repo.fetch_tasks_page(None, true, 50, 0).await.unwrap();
    assert_eq!(real_total, 1, "the fallback must be filtered out");
    assert_eq!(only_real[0]["task_id"].as_str(), Some(real.task_id.as_str()));

    // And scoping by target still works.
    let (scoped, _) = repo
        .fetch_tasks_page(Some("tgt-mix"), false, 50, 0)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 2);
    let (none, _) = repo
        .fetch_tasks_page(Some("tgt-nope"), false, 50, 0)
        .await
        .unwrap();
    assert!(none.is_empty());

    // Silence the unused warning while keeping the binding meaningful.
    assert!(!fallback.is_real_boundary());
}

// ─── Actors (Stage 12) ────────────────────────────────────────────────────────

/// Seed a sample's metadata, edges and actor nodes.
async fn seed_with_actors(
    repo: &MongoRepository,
    hash: &str,
    target: &str,
    fixture_name: &str,
) -> (SampleMetadata, Vec<logflayer::models::ActorRecord>) {
    let content = fixture(fixture_name);
    let out = Preprocessor::new(default_preprocessing_config()).run(hash, target, &content);
    repo.store_metadata(&out.metadata).await.expect("store metadata");

    GraphWriter::new(repo.destination_db())
        .write_edges(&out.metadata.relations)
        .await
        .expect("write edges");

    for actor in &out.actors {
        repo.upsert_actor(actor, actor.event_count)
            .await
            .expect("upsert actor");
    }
    (out.metadata, out.actors)
}

/// The property Stage 12 exists for: a traversal from an event must reach its
/// actors **and hydrate them**, not report them as unresolved.
///
/// This is the same failure mode the trace pseudo-node had — an edge pointing at
/// something the hydration step could not look up.
#[tokio::test]
async fn test_traversal_hydrates_actor_nodes() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let (metadata, actors) =
        seed_with_actors(&repo, "hash-act-001", "tgt-act", "openai_chat_completions.log").await;
    assert!(!actors.is_empty(), "fixture must produce actors");

    // Start from an event that references an actor.
    let actor_edge = metadata
        .relations
        .iter()
        .find(|r| format!("{:?}", r.relation_type) == "UsedSkill")
        .expect("fixture must produce a UsedSkill edge");

    let result = repo
        .traverse_graph(&actor_edge.source_entity_id, Direction::Downstream, 2, false)
        .await
        .expect("traversal");

    assert!(
        result.node_ids.contains(&actor_edge.target_entity_id),
        "the traversal must reach the actor",
    );
    assert!(
        result.unresolved_node_ids.is_empty(),
        "actor nodes must hydrate, not land in unresolved: {:?}",
        result.unresolved_node_ids,
    );
    // And the hydrated record must actually be the actor, keyed on actor_id.
    assert!(
        result.entities.iter().any(|e| {
            e.get("actor_id").and_then(|v| v.as_str()) == Some(actor_edge.target_entity_id.as_str())
        }),
        "the actor record must be among the hydrated nodes",
    );
}

/// An actor seen in two samples must be one node that accumulates, not two.
#[tokio::test]
async fn test_actor_accumulates_across_samples() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let (_, first) =
        seed_with_actors(&repo, "hash-acc-a", "tgt-acc", "openai_chat_completions.log").await;
    let (_, second) =
        seed_with_actors(&repo, "hash-acc-b", "tgt-acc", "openai_chat_completions.log").await;

    let actor_id = &first[0].actor_id;
    assert!(
        second.iter().any(|a| &a.actor_id == actor_id),
        "the same actor must be identified in both samples",
    );

    let (page, total) = repo.fetch_actors_page(None, None, 100, 0).await.unwrap();
    assert_eq!(
        total as usize,
        first.len(),
        "the second sample must not create duplicate actor documents",
    );

    let doc = page
        .iter()
        .find(|a| a["actor_id"].as_str() == Some(actor_id.as_str()))
        .expect("actor must be listed");
    assert_eq!(
        doc["sample_hashes"].as_array().unwrap().len(),
        2,
        "both samples must be recorded against the actor",
    );
}

/// Re-processing must not inflate an actor's reference count.
#[tokio::test]
async fn test_reprocessing_does_not_inflate_actor_counts() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let content = fixture("openai_chat_completions.log");
    let out = Preprocessor::new(default_preprocessing_config()).run("hash-dup-a", "t", &content);
    let actor = &out.actors[0];

    // First write.
    repo.upsert_actor(actor, actor.event_count).await.unwrap();
    let after_first = repo.fetch_actors_page(None, None, 10, 0).await.unwrap().0;
    let count_first = after_first
        .iter()
        .find(|a| a["actor_id"].as_str() == Some(actor.actor_id.as_str()))
        .unwrap()["event_count"]
        .as_i64()
        .unwrap();

    // Re-run: the guard the service applies is `actor_sample_counted`.
    let counted = repo
        .actor_sample_counted(&actor.actor_id, "hash-dup-a")
        .await
        .unwrap();
    assert!(counted, "the sample must be recognised as already recorded");
    repo.upsert_actor(actor, if counted { 0 } else { actor.event_count })
        .await
        .unwrap();

    let after_second = repo.fetch_actors_page(None, None, 10, 0).await.unwrap().0;
    let count_second = after_second
        .iter()
        .find(|a| a["actor_id"].as_str() == Some(actor.actor_id.as_str()))
        .unwrap()["event_count"]
        .as_i64()
        .unwrap();
    assert_eq!(count_second, count_first, "re-processing must not double-count");
}

/// `fetch_actors_page` must filter by kind and by task.
#[tokio::test]
async fn test_fetch_actors_page_filters() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let (metadata, actors) =
        seed_with_actors(&repo, "hash-filt", "tgt-filt", "openai_chat_completions.log").await;

    let (agents, _) = repo.fetch_actors_page(Some("agent"), None, 50, 0).await.unwrap();
    assert!(
        agents.iter().all(|a| a["kind"].as_str() == Some("agent")),
        "kind filter must be honoured",
    );
    assert_eq!(
        agents.len(),
        actors
            .iter()
            .filter(|a| format!("{:?}", a.kind) == "Agent")
            .count(),
    );

    // "which actors worked on this task"
    let (by_task, _) = repo
        .fetch_actors_page(None, Some(&metadata.task_id), 50, 0)
        .await
        .unwrap();
    assert_eq!(by_task.len(), actors.len(), "all actors belong to the task");

    let (none, _) = repo
        .fetch_actors_page(None, Some("task-nope"), 50, 0)
        .await
        .unwrap();
    assert!(none.is_empty());
}

// ─── Similarity search ────────────────────────────────────────────────────────

/// `search_embeddings` must rank a sample's own vector first at 1.0, honour the
/// self-exclusion, and report a dimensionality mismatch as skipped rather than as
/// a poor match.
#[tokio::test]
async fn test_search_embeddings_ranks_self_first_and_reports_mismatches() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    // Two structurally different samples, so their behavioral vectors differ.
    let a = seed_embeddings(&repo, "hash-emb-001", "tgt-emb", "mcp_session.log").await;
    let _b = seed_embeddings(&repo, "hash-emb-002", "tgt-emb", "openai_chat_completions.log").await;

    let query = repo
        .fetch_embedding_vector(&a, EmbeddingKind::Behavioral)
        .await
        .expect("fetch vector")
        .expect("seeded sample must have a behavioral embedding");
    assert_eq!(query.len(), 36, "behavioral vectors are 36-dimensional");

    // Including self, the query's own sample must top the ranking at 1.0.
    let with_self = repo
        .search_embeddings(EmbeddingKind::Behavioral, &query, 10, None, None)
        .await
        .expect("search");
    assert_eq!(with_self.hits[0].sample_hash, a);
    assert!(
        (with_self.hits[0].score - 1.0).abs() < 1e-5,
        "a vector against itself must score 1.0, got {}",
        with_self.hits[0].score,
    );
    assert_eq!(with_self.skipped, 0);
    assert!(!with_self.truncated);

    // Excluding self, it must be gone entirely.
    let without_self = repo
        .search_embeddings(EmbeddingKind::Behavioral, &query, 10, None, Some(&a))
        .await
        .expect("search excluding self");
    assert!(
        without_self.hits.iter().all(|h| h.sample_hash != a),
        "excluded sample leaked into the results",
    );
    assert_eq!(
        without_self.hits.len(),
        with_self.hits.len() - 1,
        "exactly one hit should disappear",
    );

    // A wrong-sized query is a caller error, not a set of bad matches: nothing
    // scores, everything is skipped.
    let mismatched = repo
        .search_embeddings(EmbeddingKind::Behavioral, &[1.0; 5], 10, None, None)
        .await
        .expect("search with wrong dimensionality");
    assert!(mismatched.hits.is_empty());
    assert_eq!(mismatched.scored, 0);
    assert!(mismatched.skipped > 0, "candidates must be counted as skipped");
}

/// The `target_id` filter must scope results, and an unknown target must yield a
/// definite empty answer rather than an unfiltered one.
#[tokio::test]
async fn test_search_embeddings_scopes_by_target() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let mine = seed_embeddings(&repo, "hash-emb-010", "tgt-mine", "mcp_session.log").await;
    let theirs = seed_embeddings(&repo, "hash-emb-011", "tgt-theirs", "mcp_session.log").await;

    let query = repo
        .fetch_embedding_vector(&mine, EmbeddingKind::Behavioral)
        .await
        .unwrap()
        .unwrap();

    let scoped = repo
        .search_embeddings(EmbeddingKind::Behavioral, &query, 10, Some("tgt-mine"), None)
        .await
        .expect("scoped search");
    assert!(
        scoped.hits.iter().all(|h| h.sample_hash != theirs),
        "the other target's sample must not appear",
    );
    assert!(scoped.hits.iter().any(|h| h.sample_hash == mine));

    // Unknown target: empty, and distinguishable from a dimensionality problem
    // because nothing was skipped either.
    let none = repo
        .search_embeddings(EmbeddingKind::Behavioral, &query, 10, Some("tgt-nope"), None)
        .await
        .expect("search on unknown target");
    assert!(none.hits.is_empty());
    assert_eq!(none.scored, 0);
    assert_eq!(none.skipped, 0);
}

/// Content embeddings are off by default, so asking for one must be a clean
/// `None` rather than an error — the handler turns that into a 400 explaining why.
#[tokio::test]
async fn test_fetch_content_embedding_is_none_when_disabled() {
    let node = Mongo::default().start().await.expect("start MongoDB container");
    let host = node.get_host().await.expect("get_host");
    let port = node.get_host_port_ipv4(27017).await.expect("get_port");

    let repo = MongoRepository::connect(&make_config(&host.to_string(), port).await)
        .await
        .expect("connect");

    let hash = seed_embeddings(&repo, "hash-emb-020", "tgt-emb", "mcp_session.log").await;

    assert!(
        repo.fetch_embedding_vector(&hash, EmbeddingKind::Behavioral)
            .await
            .unwrap()
            .is_some(),
        "behavioral embeddings need no API key and must be present",
    );
    assert!(
        repo.fetch_embedding_vector(&hash, EmbeddingKind::Content)
            .await
            .unwrap()
            .is_none(),
        "content embeddings require an API key; absence must not be an error",
    );
    assert!(
        repo.fetch_embedding_vector("no-such-sample", EmbeddingKind::Behavioral)
            .await
            .unwrap()
            .is_none(),
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
