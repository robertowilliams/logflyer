//! Preprocessing pipeline for stored log samples.
//!
//! The [`Preprocessor`] is the single entry-point for all analysis work that
//! runs after a new [`SampleRecord`] has been committed to MongoDB.  It
//! orchestrates the four sub-stages in order:
//!
//! 1. **Format detection** — [`format_detector`] classifies the log structure.
//! 2. **Stats computation** — [`stats`] derives quantitative metrics.
//! 3. **Agentic scanning** — [`agentic_scanner`] looks for LLM activity.
//! 4. **Schema extraction** — [`schema_extractor`] infers field layout for
//!    structured formats.
//! 5. **Hint derivation** — [`hints`] synthesises the above into actionable
//!    guidance for the downstream LLM classifier.
//!
//! All computation is synchronous and CPU-bound; the caller is responsible for
//! wrapping the [`Preprocessor::run`] call inside
//! `tokio::task::spawn_blocking`.

pub mod agentic_scanner;
pub mod format_detector;
pub mod hints;
pub mod schema_extractor;
pub mod stats;

use mongodb::bson::DateTime;

use crate::config::PreprocessingConfig;
use crate::models::{ClassificationStatus, SampleMetadata};

/// Current pipeline version — increment this when the output schema or logic
/// changes so that old `SampleMetadata` documents can be identified and
/// reprocessed by a future backfill job.
pub const PREPROCESSING_VERSION: &str = "1";

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
    pub fn run(&self, sample_hash: &str, target_id: &str, content: &str) -> SampleMetadata {
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

        SampleMetadata {
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
        let meta = preprocessor.run("hash-001", "target-langchain", &content);

        assert_eq!(meta.sample_hash, "hash-001");
        assert_eq!(meta.target_id, "target-langchain");
        assert_eq!(meta.preprocessing_version, "1");
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
        let meta = preprocessor.run("hash-002", "target-nginx", &content);

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
        let meta = preprocessor.run("hash-003", "target-crewai", &content);

        assert_eq!(meta.format.log_type, crate::models::LogType::Logfmt);
        assert!(meta.agentic_scan.worth_classifying, "CrewAI log should be worth classifying");
        assert!(meta.schema.is_some(), "Logfmt log should produce a schema");
        assert_eq!(meta.ingestion_hints.prompt_template, crate::models::PromptTemplate::LogfmtAgent);
    }

    #[test]
    fn test_pipeline_empty_content() {
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("hash-empty", "target-empty", "");

        assert_eq!(meta.stats.total_lines, 0);
        assert!(!meta.agentic_scan.worth_classifying);
        assert!(!meta.ingestion_hints.worth_classifying);
        assert!(meta.schema.is_none());
    }

    #[test]
    fn test_pipeline_version_string() {
        let preprocessor = Preprocessor::new(default_config());
        let meta = preprocessor.run("h", "t", "some log line");
        assert!(!meta.preprocessing_version.is_empty());
        assert_eq!(meta.preprocessing_version, PREPROCESSING_VERSION);
    }

    #[test]
    fn test_pipeline_disabled_config_does_not_affect_run() {
        // Preprocessor::run always runs — the enabled flag is checked by the caller.
        // Verify run() works regardless.
        let config = PreprocessingConfig { enabled: false, agentic_threshold: 0.02, max_schema_lines: 200 };
        let preprocessor = Preprocessor::new(config);
        let meta = preprocessor.run("h", "t", r#"{"level":"info","msg":"test"}"#);
        assert_eq!(meta.format.log_type, crate::models::LogType::Json);
    }
}
