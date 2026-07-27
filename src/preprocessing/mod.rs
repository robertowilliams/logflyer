//! Preprocessing pipeline for stored log samples.
//!
//! The [`Preprocessor`] is the single entry-point for all analysis work that
//! runs after a new [`SampleRecord`] has been committed to MongoDB.  It
//! orchestrates the pipeline stages in order:
//!
//! 1. **Format detection** — [`format_detector`] classifies the log structure.
//! 2. **Stats computation** — [`stats`] derives quantitative metrics.
//! 3. **Agentic scanning** — [`agentic_scanner`] looks for LLM activity.
//! 4. **Schema extraction** — [`schema_extractor`] infers field layout for
//!    structured formats.
//! 5. **Hint derivation** — [`hints`] synthesises the above into actionable
//!    guidance for the downstream LLM classifier.
//! 6. **Entity extraction** — [`entity_extractor`] parses typed
//!    [`EntityRecord`] instances (Phase 1).
//! 7. **Semantic classification** — [`semantic_classifier`] assigns a
//!    [`SemanticRole`] to each entity (Phase 2).
//! 8. **Relation extraction** — [`relation_extractor`] builds directed
//!    [`RelationEdge`] records (Phase 3).
//! 9. **PROV-O linking** — [`prov_linker`] emits W3C PROV triples (Phase 4).
//! 10. **OTel span assembly** — [`otel_builder`] builds span records (Phase 4).
//!
//! OTel trace IDs are generated here (one per sample) so all downstream
//! phases share the same root span identifier.
//!
//! All computation is synchronous and CPU-bound; the caller is responsible for
//! wrapping the [`Preprocessor::run`] call inside
//! `tokio::task::spawn_blocking`.

pub mod actor_extractor;
pub mod agentic_scanner;
pub mod entity_extractor;
pub mod format_detector;
pub mod hints;
pub mod ids;
pub mod mcp_parser;
pub mod otel_builder;
pub mod prov_linker;
pub mod relation_extractor;
pub mod schema_extractor;
pub mod semantic_classifier;
pub mod stats;
pub mod task_correlator;
pub mod task_intent;

use mongodb::bson::DateTime;
use tracing::warn;

use crate::config::PreprocessingConfig;
use crate::models::{ClassificationStatus, LogType, SampleMetadata};
use crate::preprocessing::otel_builder::OtelSpan;
use crate::preprocessing::prov_linker::ProvTriple;

/// Full output of a single pipeline run.
///
/// `metadata` is the document persisted to the `sample_metadata` collection;
/// `prov_triples` and `otel_spans` are computed alongside but persisted by
/// the async output adapters (`output::graph`) rather than inlined on
/// `SampleMetadata` to keep the document compact.
#[derive(Debug)]
pub struct PipelineOutput {
    pub metadata:     SampleMetadata,
    pub prov_triples: Vec<ProvTriple>,
    pub otel_spans:   Vec<OtelSpan>,
    /// Task this sample was correlated into (Stage 11), or `None` when task
    /// correlation is disabled. Carried out separately from `metadata` because the
    /// `tasks` collection is upserted by the caller, not written inline.
    pub task_correlation: Option<task_correlator::TaskCorrelation>,
    /// Actors and their edges from Stage 12, or `None` when actor nodes are
    /// disabled. The edges are *also* folded into `metadata.relations`; the
    /// records are carried here for the caller to persist separately.
    pub actors: Vec<crate::models::ActorRecord>,
    /// The task's goal statement (Stage 13), when one could be identified.
    /// `None` when task correlation is off, or the sample states no goal — in
    /// which case the task simply has no intent embedding rather than a
    /// misleading one built from an arbitrary line.
    pub task_intent: Option<String>,
}

/// Current pipeline version — increment this when the output schema or logic
/// changes so that old `SampleMetadata` documents can be identified and
/// reprocessed by a future backfill job.
pub const PREPROCESSING_VERSION: &str = "2";

/// Synchronous preprocessing pipeline.
///
/// Construct once (cheaply) and call [`Preprocessor::run`] for each sample.
/// The struct holds only the configuration, so it is safe to clone or share
/// across threads.
#[derive(Clone)]
pub struct Preprocessor {
    config: PreprocessingConfig,
}

impl Preprocessor {
    pub fn new(config: PreprocessingConfig) -> Self {
        Self { config }
    }

    /// Run the full pipeline for a single sample.
    ///
    /// `sample_hash` and `target_id` are stored verbatim in the returned
    /// [`SampleMetadata`] so callers do not need to re-derive them.
    ///
    /// This method is **synchronous** and may perform regex matching and JSON
    /// parsing on the full sample content.  Call it inside
    /// `tokio::task::spawn_blocking` when integrating with an async runtime.
    ///
    /// Returns a [`PipelineOutput`] that bundles the [`SampleMetadata`] with
    /// the derived PROV triples and OTel spans.  The metadata is persisted by
    /// the caller; the triples and spans are passed to the async output
    /// adapters (`output::graph`) when enabled.
    pub fn run(&self, sample_hash: &str, target_id: &str, content: &str) -> PipelineOutput {
        // Derive a W3C-compatible 128-bit OTel trace ID (32 hex chars) from the
        // stable `sample_hash` so re-running the pipeline produces the same
        // trace, and downstream OTel exporters can join on trace_id without
        // first looking up the sample.
        let otel_trace_id = ids::derive_trace_id(sample_hash);

        // Stage 1: format detection
        let format = format_detector::detect(content);

        // Stage 2: statistics
        let stats = stats::compute(content, &format.log_type);

        // Stage 3: agentic signal scan
        let agentic_scan = agentic_scanner::scan(content, self.config.agentic_threshold);

        // Stage 4: schema extraction (structured formats only)
        let schema = schema_extractor::extract(
            content,
            &format.log_type,
            self.config.max_schema_lines,
        );

        // Stage 5: ingestion hints
        let ingestion_hints = hints::derive(&format, &stats, &agentic_scan);

        // ── Stages 6–10 are gated by `entity_extraction_enabled` so the new
        //    pipeline can be rolled out incrementally without touching the
        //    classification path that depends only on Stages 1–5. ──────────────
        let (entities, relations, prov_triples, otel_spans) =
            if self.config.entity_extraction_enabled {
                // Stage 6: entity extraction — parses log lines into typed
                // `EntityRecord` instances with fields, span IDs, and parent
                // links.
                let mut entities = entity_extractor::extract(
                    content,
                    &format,
                    &schema,
                    &agentic_scan,
                    sample_hash,
                    target_id,
                    &otel_trace_id,
                );

                // Stage 7: semantic classification — assigns a `SemanticRole`
                // to each entity in place using entity type + raw-text
                // disambiguators.
                semantic_classifier::classify(&mut entities);

                // Stage 8: relation extraction — directed `RelationEdge`
                // records between entities (e.g. ToolCallEvent →
                // ToolResultEvent).
                let relations = relation_extractor::extract(&entities, sample_hash);

                // Stage 9: PROV-O triple generation.  Returned in
                // `PipelineOutput.prov_triples` so the async graph adapter can
                // persist them without re-running the pipeline.
                let prov_triples = prov_linker::build(&entities, &relations, sample_hash);

                // Stage 10: OTel span assembly.  Same rationale as Stage 9.
                let otel_spans = otel_builder::build(&entities, sample_hash);

                (entities, relations, prov_triples, otel_spans)
            } else {
                (Vec::new(), Vec::new(), Vec::new(), Vec::new())
            };

        // ── Stage 11: task correlation ────────────────────────────────────────
        // Scans the whole sample for a correlation key rather than reading the
        // extracted entities, because keys routinely sit on lines that never become
        // entities — crewai's `task_id` is on `msg="Task assigned"` lines, which
        // match no entity pattern. See `task_correlator::correlate`.
        //
        // Independent of `entity_extraction_enabled` for the same reason: a sample
        // still belongs to a task even when no entities were extracted from it.
        //
        // Additive: this stamps `task_id` onto entities and touches nothing else,
        // so `trace_id`, span ids, entity ids and relation ids keep their values.
        let mut entities = entities;
        let task_correlation = if self.config.task_correlation_enabled {
            let correlation = task_correlator::correlate(
                content,
                format.log_type == LogType::Json,
                sample_hash,
                target_id,
            );
            if !correlation.spanning_keys.is_empty() {
                // Not an error, but the boundary is coarser than the log could
                // have supported — worth saying out loud rather than burying.
                warn!(
                    sample_hash,
                    keys = ?correlation.spanning_keys,
                    chosen = %correlation.source,
                    "sample spans several values of a correlation key; \
                     it contains more than one unit of work"
                );
            }
            task_correlator::apply(&mut entities, &correlation);
            Some(correlation)
        } else {
            None
        };

        // ── Stage 12: actor nodes ─────────────────────────────────────────────
        // Promotes the participants buried in event attributes — model_id,
        // tool_name, mcp_server_id — into their own nodes, with edges from the
        // events that involved them.
        //
        // Runs after task correlation so actors can record which task they took
        // part in. The edges join the sample's other relations and are persisted
        // by the same graph writer; the actor records go to their own collection.
        let mut relations = relations;
        let actor_extraction = if self.config.actor_nodes_enabled {
            let task_id = task_correlation
                .as_ref()
                .map(|c| c.task_id.as_str())
                .unwrap_or("");
            let extraction = actor_extractor::extract(&entities, sample_hash, task_id);
            relations.extend(extraction.edges.iter().cloned());
            Some(extraction)
        } else {
            None
        };

        // ── Stage 13: task intent ─────────────────────────────────────────────
        // The sentence stating what this task was for, to be embedded on its own.
        // Only meaningful once a task exists, so it rides with correlation.
        let task_intent = if task_correlation.is_some() {
            task_intent::extract(&entities)
        } else {
            None
        };

        let entity_count = entities.len() as u32;
        let relation_count = relations.len() as u32;

        let metadata = SampleMetadata {
            sample_hash: sample_hash.to_string(),
            target_id: target_id.to_string(),
            analyzed_at: DateTime::now(),
            preprocessing_version: PREPROCESSING_VERSION.to_string(),
            format,
            stats,
            agentic_scan,
            schema,
            ingestion_hints,
            classification_status: ClassificationStatus::Pending,
            otel_trace_id,
            entities,
            relations,
            entity_count,
            relation_count,
            task_id: task_correlation
                .as_ref()
                .map(|c| c.task_id.clone())
                .unwrap_or_default(),
            task_id_source: task_correlation
                .as_ref()
                .map(|c| c.source.clone())
                .unwrap_or_default(),
        };

        PipelineOutput {
            metadata,
            prov_triples,
            otel_spans,
            task_correlation,
            actors: actor_extraction.map(|x| x.actors).unwrap_or_default(),
            task_intent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClassificationStatus;

    fn default_config() -> PreprocessingConfig {
        PreprocessingConfig {
            enabled: true,
            agentic_threshold: 0.02,
            max_schema_lines: 200,
            metrics_port: 9090,
            entity_extraction_enabled: true,
            min_entities_for_persist: 1,
            task_correlation_enabled: true,
            actor_nodes_enabled: true,
        }
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
    }

    #[test]
    fn test_pipeline_langchain_json_end_to_end() {
        let content = fixture("langchain_json.log");
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("hash-001", "target-langchain", &content).metadata;

        assert_eq!(meta.sample_hash, "hash-001");
        assert_eq!(meta.target_id, "target-langchain");
        assert_eq!(meta.preprocessing_version, "2");
        assert_eq!(meta.classification_status, ClassificationStatus::Pending);

        // Format
        assert_eq!(meta.format.log_type, crate::models::LogType::Json);

        // Stats
        assert!(meta.stats.total_lines > 0);
        assert!(meta.stats.non_empty_lines > 0);

        // Agentic scan should flag this as worth classifying
        assert!(
            meta.agentic_scan.worth_classifying,
            "LangChain log should be worth classifying, score={}",
            meta.agentic_scan.signal_score
        );

        // Schema should be present for JSON
        assert!(meta.schema.is_some(), "JSON log should produce a schema");

        // Hints
        assert!(meta.ingestion_hints.worth_classifying);
        assert!(meta.ingestion_hints.suggested_chunk_size > 0);
    }

    #[test]
    fn test_pipeline_nginx_skipped() {
        let content = fixture("nginx_access.log");
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("hash-002", "target-nginx", &content).metadata;

        // Nginx is not agentic — should not be worth classifying
        assert!(
            !meta.agentic_scan.worth_classifying || meta.agentic_scan.signal_score < 0.1,
            "nginx should not be worth classifying, score={}",
            meta.agentic_scan.signal_score
        );
        // No JSON schema expected for plain-text nginx logs
        assert!(meta.schema.is_none(), "nginx log should produce no schema");
    }

    #[test]
    fn test_pipeline_crewai_logfmt_end_to_end() {
        let content = fixture("crewai_logfmt.log");
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("hash-003", "target-crewai", &content).metadata;

        assert_eq!(meta.format.log_type, crate::models::LogType::Logfmt);
        assert!(meta.agentic_scan.worth_classifying, "CrewAI log should be worth classifying");
        assert!(meta.schema.is_some(), "Logfmt log should produce a schema");
        assert_eq!(meta.ingestion_hints.prompt_template, crate::models::PromptTemplate::LogfmtAgent);
    }

    #[test]
    fn test_pipeline_empty_content() {
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("hash-empty", "target-empty", "").metadata;

        assert_eq!(meta.stats.total_lines, 0);
        assert!(!meta.agentic_scan.worth_classifying);
        assert!(!meta.ingestion_hints.worth_classifying);
        assert!(meta.schema.is_none());
    }

    #[test]
    fn test_pipeline_version_string() {
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("h", "t", "some log line").metadata;
        assert!(!meta.preprocessing_version.is_empty());
        assert_eq!(meta.preprocessing_version, PREPROCESSING_VERSION);
    }

    #[test]
    fn test_pipeline_disabled_config_does_not_affect_run() {
        // Preprocessor::run always runs — the enabled flag is checked by the caller.
        // Verify run() works regardless.
        let config = PreprocessingConfig {
            enabled: false,
            agentic_threshold: 0.02,
            max_schema_lines: 200,
            metrics_port: 9090,
            entity_extraction_enabled: true,
            min_entities_for_persist: 1,
            task_correlation_enabled: true,
            actor_nodes_enabled: true,
        };
        let preprocessor = Preprocessor::new(config);
        let meta = preprocessor
            .run("h", "t", r#"{"level":"info","msg":"test"}"#)
            .metadata;
        assert_eq!(meta.format.log_type, crate::models::LogType::Json);
    }

    #[test]
    fn test_pipeline_entity_extraction_disabled_skips_stages_6_to_10() {
        let mut config = default_config();
        config.entity_extraction_enabled = false;
        let preprocessor = Preprocessor::new(config);
        // A clearly agentic line — would normally produce entities/relations.
        let line = r#"{"role":"assistant","content":"hi","finish_reason":"stop"}"#;
        let out = preprocessor.run("h", "t", line);

        // Stages 1-5 still ran.
        assert_eq!(out.metadata.format.log_type, crate::models::LogType::Json);
        // Stages 6-10 produced no output.
        assert!(out.metadata.entities.is_empty(), "entities should be empty when disabled");
        assert!(out.metadata.relations.is_empty(), "relations should be empty when disabled");
        assert_eq!(out.metadata.entity_count, 0);
        assert_eq!(out.metadata.relation_count, 0);
        assert!(out.prov_triples.is_empty(), "prov_triples should be empty when disabled");
        assert!(out.otel_spans.is_empty(), "otel_spans should be empty when disabled");
    }

    #[test]
    fn test_pipeline_entity_extraction_enabled_returns_prov_and_otel() {
        let preprocessor = Preprocessor::new(default_config());
        let line = r#"{"role":"assistant","content":"hi","finish_reason":"stop"}"#;
        let out = preprocessor.run("h", "t", line);

        // We expect at least one entity (CompletionEvent) and a corresponding
        // OTel span — verifying the new return type carries the data through.
        assert!(!out.metadata.entities.is_empty());
        assert!(!out.otel_spans.is_empty(), "otel_spans should be emitted");
        // PROV triples include at least the per-entity attribution edges.
        assert!(!out.prov_triples.is_empty(), "prov_triples should be emitted");
    }

    // ── Span status against real fixtures ────────────────────────────────────
    //
    // `otel_builder::status` is unit-tested against hand-built field maps, but
    // those maps are only useful if the extractor actually produces that shape.
    // These tests run whole fixtures through the pipeline so a mismatch between
    // `status()`'s expectations and `extracted_fields`' real contents shows up
    // as a failure rather than as silently-Unset spans.

    #[test]
    fn openai_fixture_yields_an_ok_span_from_nested_finish_reason() {
        use crate::preprocessing::otel_builder::StatusCode;

        let content = fixture("openai_chat_completions.log");
        let out = Preprocessor::new(default_config()).run("h-openai", "t", &content);

        assert!(
            out.otel_spans.iter().any(|s| s.status.code == StatusCode::Ok),
            "the OpenAI fixture carries choices[0].finish_reason=stop, so at least \
             one span must be affirmatively Ok — all-Unset means status() is not \
             seeing the shape the extractor produces",
        );
    }

    #[test]
    fn mcp_fixture_yields_an_error_span_from_the_jsonrpc_envelope() {
        use crate::preprocessing::otel_builder::StatusCode;

        let content = fixture("mcp_session.log");
        let out = Preprocessor::new(default_config()).run("h-mcp", "t", &content);

        let errors: Vec<_> = out
            .otel_spans
            .iter()
            .filter(|s| s.status.code == StatusCode::Error)
            .collect();
        assert!(
            !errors.is_empty(),
            "the MCP fixture contains a JSON-RPC error envelope, so a span must be Error",
        );
        assert!(
            errors.iter().any(|s| s.status.message.contains("-32602")),
            "the error code belongs in the status message, got: {:?}",
            errors.iter().map(|s| &s.status.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn bedrock_multiline_fixture_yields_an_error_from_plaintext_severity() {
        use crate::preprocessing::otel_builder::StatusCode;

        let content = fixture("bedrock_multiline.log");
        let out = Preprocessor::new(default_config()).run("h-bedrock", "t", &content);

        // Plain-text lines produce no key/value pairs, so this can only pass via
        // the raw_text severity fallback.
        if !out.otel_spans.is_empty() {
            assert!(
                out.otel_spans.iter().any(|s| s.status.code == StatusCode::Error),
                "the bedrock fixture has an `ERROR ... ThrottlingException` line",
            );
        }
    }

    // ── Stage 11: task correlation against real fixtures ─────────────────────
    //
    // The unit tests in `task_correlator` build field maps by hand. These run whole
    // fixtures through the pipeline, so a mismatch between what the correlator
    // expects and what the extractor actually produces shows up as a failure
    // rather than as everything silently falling back to sample scope.

    #[test]
    fn langchain_fixture_correlates_on_a_real_key() {
        let content = fixture("langchain_json.log");
        let out = Preprocessor::new(default_config()).run("h-lc", "t", &content);

        assert!(
            !out.metadata.task_id.is_empty(),
            "task_id must be populated when correlation is enabled",
        );
        assert_ne!(
            out.metadata.task_id_source, "sample",
            "langchain carries session_id/run_id, so this must not be a fallback",
        );
        // Both session_id and run_id are present, and `run_id` outranks
        // `session_id` — but it appears on exactly one line (18, the last) while
        // `session_id` spans the sample. Coverage decides, so the session wins.
        //
        // This is the regression that matters: under first-hit-of-highest-rank a
        // sample cut at line 17 correlated to the session and one cut at line 18
        // to the run, so the task id depended on where the sampler cut and two
        // overlapping samples of one run could never be joined.
        assert_eq!(out.metadata.task_id_source, "session_id");

        let correlation = out.task_correlation.expect("correlation must be returned");
        assert!(correlation.is_real_boundary());
        assert!(correlation.correlation_key.is_some());
        assert!(
            out.metadata.entities.iter().all(|e| e.task_id == correlation.task_id),
            "every entity must be stamped with the task id",
        );
    }

    #[test]
    fn crewai_fixture_declines_to_pick_one_of_its_two_tasks() {
        let content = fixture("crewai_logfmt.log");
        let out = Preprocessor::new(default_config()).run("h-crew", "t", &content);
        let correlation = out.task_correlation.expect("correlation must be returned");

        // The fixture genuinely contains two units of work — `task_id=task-1`
        // (researcher) and `task_id=task-2` (writer, then reviewer). Taking the
        // first stamped `task-1` onto the second task's events and reported
        // `is_real_boundary() == true`: a specific, confident, wrong attribution
        // that then propagated into `actors.task_ids` and the task's
        // `sample_hashes`.
        assert_ne!(out.metadata.task_id_source, "task_id");
        assert_eq!(
            correlation.spanning_keys,
            vec!["task_id".to_string()],
            "the sample must record that it spans several task_id values",
        );

        // Having declined the key the sample disagrees on, it falls through to
        // one the whole sample does agree on.
        assert_eq!(out.metadata.task_id_source, "crew_id");
        assert!(correlation.is_real_boundary());
    }

    #[test]
    fn bedrock_fixture_spans_three_requests_and_says_so() {
        // Three distinct `request_id`s in one sample. The old first-hit rule
        // collapsed them onto `req-bedrock-001` silently; the sample must now
        // report that it spans them rather than picking one.
        let content = fixture("bedrock_multiline.log");
        let out = Preprocessor::new(default_config()).run("h-bedrock", "t", &content);
        let correlation = out.task_correlation.expect("correlation must be returned");

        assert!(
            correlation.spanning_keys.contains(&"request_id".to_string()),
            "got {:?}",
            correlation.spanning_keys,
        );
        assert_ne!(out.metadata.task_id_source, "request_id");
        // Nothing coarser to fall through to, so it says "we could not tell"
        // instead of asserting a boundary it does not have.
        assert_eq!(out.metadata.task_id_source, "sample");
        assert!(!correlation.is_real_boundary());
    }

    #[test]
    fn mcp_fixture_falls_back_cleanly() {
        // Raw JSON-RPC has no session concept at all. The fallback must be clean
        // and clearly labelled, not an empty or bogus task id.
        let content = fixture("mcp_session.log");
        let out = Preprocessor::new(default_config()).run("h-mcp", "t", &content);

        assert_eq!(out.metadata.task_id_source, "sample");
        assert_eq!(out.metadata.task_id.len(), 32);
        let correlation = out.task_correlation.expect("a fallback is still a correlation");
        assert!(!correlation.is_real_boundary());
        assert_eq!(correlation.correlation_key, None);
    }

    #[test]
    fn the_same_correlation_key_groups_two_samples() {
        // The property the whole phase exists for: two different samples of the
        // same session must land in one task.
        let content = fixture("langchain_json.log");
        let pre = Preprocessor::new(default_config());
        let a = pre.run("hash-one", "t", &content);
        let b = pre.run("hash-two", "t", &content);

        assert_ne!(a.metadata.sample_hash, b.metadata.sample_hash);
        assert_eq!(
            a.metadata.task_id, b.metadata.task_id,
            "same session across two samples must be one task",
        );
        // And the sample-scoped ids must still differ, proving nothing else moved.
        assert_ne!(a.metadata.otel_trace_id, b.metadata.otel_trace_id);
    }

    #[test]
    fn different_samples_without_keys_stay_separate() {
        let content = fixture("mcp_session.log");
        let pre = Preprocessor::new(default_config());
        let a = pre.run("hash-one", "t", &content);
        let b = pre.run("hash-two", "t", &content);
        assert_ne!(
            a.metadata.task_id, b.metadata.task_id,
            "with no correlation key, each sample is its own task",
        );
    }

    #[test]
    fn task_correlation_disabled_leaves_everything_empty() {
        let mut config = default_config();
        config.task_correlation_enabled = false;
        let content = fixture("langchain_json.log");
        let out = Preprocessor::new(config).run("h", "t", &content);

        assert!(out.metadata.task_id.is_empty());
        assert!(out.metadata.task_id_source.is_empty());
        assert!(out.task_correlation.is_none());
        assert!(out.metadata.entities.iter().all(|e| e.task_id.is_empty()));
    }

    #[test]
    fn task_correlation_does_not_disturb_the_existing_ids() {
        // The safety property behind the whole design: adding task_id must not
        // change trace_id, span ids, entity ids or relation ids, because
        // otel_spans and PART_OF edges are keyed on them.
        let content = fixture("langchain_json.log");
        let mut off = default_config();
        off.task_correlation_enabled = false;
        let without = Preprocessor::new(off).run("h-same", "t", &content);
        let with = Preprocessor::new(default_config()).run("h-same", "t", &content);

        assert_eq!(without.metadata.otel_trace_id, with.metadata.otel_trace_id);
        assert_eq!(
            without.metadata.entities.iter().map(|e| &e.entity_id).collect::<Vec<_>>(),
            with.metadata.entities.iter().map(|e| &e.entity_id).collect::<Vec<_>>(),
        );
        assert_eq!(
            without.metadata.entities.iter().map(|e| &e.span_id).collect::<Vec<_>>(),
            with.metadata.entities.iter().map(|e| &e.span_id).collect::<Vec<_>>(),
        );
        assert_eq!(
            without.metadata.relations.iter().map(|r| &r.relation_id).collect::<Vec<_>>(),
            with.metadata.relations.iter().map(|r| &r.relation_id).collect::<Vec<_>>(),
        );
        assert_eq!(
            without.otel_spans.iter().map(|s| (&s.trace_id, &s.span_id)).collect::<Vec<_>>(),
            with.otel_spans.iter().map(|s| (&s.trace_id, &s.span_id)).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn nginx_fixture_yields_a_fallback_task_without_entities() {
        // Zero entities, so nothing to correlate on — must not panic, and must
        // still produce a usable fallback id.
        let content = fixture("nginx_access.log");
        let out = Preprocessor::new(default_config()).run("h-nginx", "t", &content);
        assert!(out.metadata.entities.is_empty());
        assert_eq!(out.metadata.task_id_source, "sample");
        assert_eq!(out.metadata.task_id.len(), 32);
    }

    // ── Stage 12: actor nodes against real fixtures ──────────────────────────

    #[test]
    fn openai_fixture_yields_agent_and_skill_nodes() {
        use crate::models::ActorKind;

        let content = fixture("openai_chat_completions.log");
        let out = Preprocessor::new(default_config()).run("h-actors", "t", &content);

        assert!(!out.actors.is_empty(), "the fixture names models and tools");
        assert!(
            out.actors.iter().any(|a| a.kind == ActorKind::Agent),
            "a model_id must become an Agent node, got {:?}",
            out.actors.iter().map(|a| (a.kind, &a.name)).collect::<Vec<_>>(),
        );
        assert!(
            out.actors.iter().any(|a| a.kind == ActorKind::Skill),
            "a tool_name must become a Skill node",
        );
        // Every actor is referenced by at least one event.
        assert!(out.actors.iter().all(|a| a.event_count > 0));
    }

    #[test]
    fn actor_edges_join_the_sample_relations() {
        use crate::models::RelationType;

        let content = fixture("openai_chat_completions.log");
        let out = Preprocessor::new(default_config()).run("h-edges", "t", &content);

        let actor_edges: Vec<_> = out
            .metadata
            .relations
            .iter()
            .filter(|r| {
                matches!(
                    r.relation_type,
                    RelationType::PerformedBy
                        | RelationType::UsedSkill
                        | RelationType::AccessedResource
                )
            })
            .collect();
        assert!(!actor_edges.is_empty(), "actor edges must reach metadata.relations");

        // Every actor edge must point at an actor that actually exists, or the
        // graph gains unlabelled nodes — the failure mode fixed for PART_OF.
        let actor_ids: std::collections::HashSet<&str> =
            out.actors.iter().map(|a| a.actor_id.as_str()).collect();
        for edge in &actor_edges {
            assert!(
                actor_ids.contains(edge.target_entity_id.as_str()),
                "edge targets {} which is not an emitted actor",
                edge.target_entity_id,
            );
        }
    }

    #[test]
    fn actors_do_not_become_otel_spans() {
        // The reason actors are not EntityType variants: otel_builder emits one
        // span per entity, so actors-as-entities would fabricate a span for every
        // agent and tool.
        let content = fixture("openai_chat_completions.log");
        let out = Preprocessor::new(default_config()).run("h-spans", "t", &content);

        assert!(!out.actors.is_empty(), "fixture must produce actors for this to mean anything");
        assert_eq!(
            out.otel_spans.len(),
            out.metadata.entities.len(),
            "one span per event, and none for actors",
        );
    }

    #[test]
    fn the_same_actor_is_one_node_across_samples() {
        let content = fixture("openai_chat_completions.log");
        let pre = Preprocessor::new(default_config());
        let a = pre.run("hash-one", "t", &content);
        let b = pre.run("hash-two", "t", &content);

        let a_ids: std::collections::HashSet<&str> =
            a.actors.iter().map(|x| x.actor_id.as_str()).collect();
        let b_ids: std::collections::HashSet<&str> =
            b.actors.iter().map(|x| x.actor_id.as_str()).collect();
        assert_eq!(a_ids, b_ids, "actor ids must be stable across samples");
    }

    #[test]
    fn actor_nodes_disabled_changes_nothing() {
        let content = fixture("openai_chat_completions.log");
        let mut off = default_config();
        off.actor_nodes_enabled = false;
        let without = Preprocessor::new(off).run("h-same", "t", &content);
        let with = Preprocessor::new(default_config()).run("h-same", "t", &content);

        assert!(without.actors.is_empty());
        assert!(
            with.metadata.relations.len() > without.metadata.relations.len(),
            "enabling actors must add edges",
        );
        // But it must not disturb the pre-existing ones.
        let base: Vec<&String> = without.metadata.relations.iter().map(|r| &r.relation_id).collect();
        let extended: Vec<&String> = with.metadata.relations.iter().map(|r| &r.relation_id).collect();
        assert!(
            base.iter().all(|id| extended.contains(id)),
            "existing relation ids must survive unchanged",
        );
        assert_eq!(without.otel_spans.len(), with.otel_spans.len());
    }

    #[test]
    fn actors_record_their_task() {
        let content = fixture("langchain_json.log");
        let out = Preprocessor::new(default_config()).run("h-at", "t", &content);
        let task_id = &out.metadata.task_id;
        assert!(!task_id.is_empty());
        assert!(
            out.actors.iter().all(|a| a.task_ids.contains(task_id)),
            "each actor must record the task it participated in",
        );
    }

    #[test]
    fn nginx_fixture_produces_no_actors() {
        // No entities, so no participants to promote.
        let content = fixture("nginx_access.log");
        let out = Preprocessor::new(default_config()).run("h-nginx-a", "t", &content);
        assert!(out.actors.is_empty());
    }

    #[test]
    fn nginx_fixture_does_not_invent_error_spans() {
        use crate::preprocessing::otel_builder::StatusCode;

        // A plain access log is non-agentic; whatever spans it yields must not be
        // spuriously marked failed by a stray `error`-looking token in a URL.
        let content = fixture("nginx_access.log");
        let out = Preprocessor::new(default_config()).run("h-nginx", "t", &content);

        assert!(
            out.otel_spans.iter().all(|s| s.status.code != StatusCode::Error),
            "no nginx access-log span should be Error",
        );
    }
}
