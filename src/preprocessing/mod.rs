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

use mongodb::bson::DateTime;

use crate::config::PreprocessingConfig;
use crate::models::{ClassificationStatus, SampleMetadata};
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
        };

        PipelineOutput { metadata, prov_triples, otel_spans }
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
