use std::collections::HashMap;

use mongodb::bson::{self, doc, Bson, DateTime, Document};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::sampling::SamplingMode;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RawTargetDocument {
    #[serde(rename = "_id")]
    pub mongo_id: Option<Bson>,
    pub target_id: Option<String>,
    pub status: Option<String>,
    #[serde(alias = "hostname", alias = "server", alias = "ip")]
    pub host: Option<String>,
    #[serde(alias = "ssh_port")]
    pub port: Option<u16>,
    #[serde(alias = "user")]
    pub username: Option<String>,
    pub password: Option<String>,
    pub private_key: Option<String>,
    #[serde(alias = "private_key_file")]
    pub private_key_path: Option<String>,
    pub private_key_passphrase: Option<String>,
    #[serde(
        alias = "log_dirs",
        alias = "log_directories",
        alias = "folders",
        alias = "paths"
    )]
    pub log_paths: Option<Vec<String>>,
    pub connection: Option<RawConnection>,
    pub credentials: Option<RawCredentials>,
    /// Per-target override: how many lines to sample from each log file.
    /// Falls back to the global `SAMPLE_LINE_COUNT` env var when absent.
    pub sample_line_count: Option<u32>,
    /// Per-target override: max number of files to discover per log directory.
    /// Falls back to the global `REMOTE_MAX_FILES_PER_TARGET` env var when absent.
    pub max_files: Option<u32>,
}

impl RawTargetDocument {
    pub fn from_document(document: Document) -> Result<Self, AppError> {
        bson::from_document(document).map_err(|error| {
            AppError::Validation(format!("failed to deserialize target document: {error}"))
        })
    }

    pub fn document_id(&self) -> String {
        self.mongo_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<missing _id>".to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RawConnection {
    #[serde(alias = "hostname", alias = "server", alias = "ip")]
    pub host: Option<String>,
    #[serde(alias = "ssh_port")]
    pub port: Option<u16>,
    #[serde(alias = "user")]
    pub username: Option<String>,
    #[serde(
        alias = "log_dirs",
        alias = "log_directories",
        alias = "folders",
        alias = "paths"
    )]
    pub log_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RawCredentials {
    pub auth_method: Option<String>,
    pub password: Option<String>,
    pub private_key: Option<String>,
    #[serde(alias = "private_key_file")]
    pub private_key_path: Option<String>,
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedTarget {
    pub document_id: String,
    pub target_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub log_paths: Vec<String>,
    /// Overrides `SamplingConfig::line_count` for this target only.
    pub sample_line_count: Option<usize>,
    /// Overrides `DiscoveryConfig::max_files_per_target` for this target only.
    pub max_files: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum AuthMethod {
    Password {
        password: String,
    },
    PrivateKeyPath {
        private_key_path: String,
        passphrase: Option<String>,
    },
    PrivateKeyInline {
        private_key: String,
        passphrase: Option<String>,
    },
    /// No credentials were provided in the target document. The connector will
    /// attempt SSH-agent authentication first, then fall back to unauthenticated
    /// access ("none" method) for servers that permit it.
    None,
}

impl ValidatedTarget {
    pub fn validate(raw: RawTargetDocument) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let document_id = raw.document_id();
        // Targets may provide connection details at the top level or under nested
        // `connection`/`credentials` objects, so validation merges the supported aliases
        // into one canonical struct before any SSH work begins.
        let target_id = take_string(raw.target_id.as_deref())
            .ok_or_else(|| "missing `target_id`".to_string())
            .map_err(|error| errors.push(error))
            .ok();

        let status = take_string(raw.status.as_deref()).unwrap_or_default();
        if !status.eq_ignore_ascii_case("active") {
            errors.push(format!("target status is `{status}` instead of `active`"));
        }

        let host = first_non_empty(&[
            raw.host.as_deref(),
            raw.connection
                .as_ref()
                .and_then(|value| value.host.as_deref()),
        ])
        .ok_or_else(|| "missing remote host".to_string())
        .map_err(|error| errors.push(error))
        .ok();

        let port = raw
            .port
            .or_else(|| raw.connection.as_ref().and_then(|value| value.port))
            .unwrap_or(22);

        let username = first_non_empty(&[
            raw.username.as_deref(),
            raw.connection
                .as_ref()
                .and_then(|value| value.username.as_deref()),
        ])
        .ok_or_else(|| "missing SSH username".to_string())
        .map_err(|error| errors.push(error))
        .ok();

        let log_paths = raw
            .log_paths
            .clone()
            .or_else(|| {
                raw.connection
                    .as_ref()
                    .and_then(|value| value.log_paths.clone())
            })
            .unwrap_or_default()
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();

        if log_paths.is_empty() {
            errors.push("missing at least one log directory path".to_string());
        }

        let auth_method = match determine_auth_method(&raw) {
            Ok(auth) => Some(auth),
            Err(error) => {
                errors.push(error);
                None
            }
        };

        if let Some(ref collection_name) = target_id {
            if !is_valid_collection_name(collection_name) {
                errors.push(format!(
                    "target_id `{collection_name}` is not a valid MongoDB collection name"
                ));
            }
        }

        if errors.is_empty() {
            Ok(Self {
                document_id,
                target_id: target_id.expect("validated target_id"),
                host: host.expect("validated host"),
                port,
                username: username.expect("validated username"),
                auth: auth_method.expect("validated auth"),
                log_paths,
                sample_line_count: raw.sample_line_count.map(|v| v as usize),
                max_files: raw.max_files.map(|v| v as usize),
            })
        } else {
            Err(errors)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    Stored,
    Empty,
    Error,
    MissingDirectory,
    NoFilesFound,
}

impl ProcessingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingStatus::Stored => "stored",
            ProcessingStatus::Empty => "empty",
            ProcessingStatus::Error => "error",
            ProcessingStatus::MissingDirectory => "missing_directory",
            ProcessingStatus::NoFilesFound => "no_files_found",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleRecord {
    pub timestamp: DateTime,
    pub target_id: String,
    pub source_file: String,
    pub sample_content: String,
    pub host: String,
    pub path: String,
    pub sampling_mode: SamplingMode,
    pub line_count: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub processing_status: ProcessingStatus,
    pub error_details: Option<String>,
    pub sample_hash: String,
}

impl SampleRecord {
    pub fn to_document(&self) -> Document {
        doc! {
            "timestamp": self.timestamp,
            "target_id": &self.target_id,
            "source_file": &self.source_file,
            "sample_content": &self.sample_content,
            "host": &self.host,
            "path": &self.path,
            "sampling_mode": self.sampling_mode.as_str(),
            "line_count": self.line_count.map(|value| value as i64),
            "file_size_bytes": self.file_size_bytes.map(|value| value as i64),
            "processing_status": self.processing_status.as_str(),
            "error_details": &self.error_details,
            "sample_hash": &self.sample_hash,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SampleDraft {
    pub target_id: String,
    pub source_file: String,
    pub sample_content: String,
    pub host: String,
    pub path: String,
    pub sampling_mode: SamplingMode,
    pub line_count: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub processing_status: ProcessingStatus,
    pub error_details: Option<String>,
}

// ─── Preprocessing output types ──────────────────────────────────────────────

/// Broad structural category of a log file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogType {
    Json,
    Logfmt,
    Syslog,
    Multiline,
    PlainText,
}

impl LogType {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogType::Json => "json",
            LogType::Logfmt => "logfmt",
            LogType::Syslog => "syslog",
            LogType::Multiline => "multiline",
            LogType::PlainText => "plain_text",
        }
    }
}

/// Detected structural format with optional field hints for well-known log types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogFormat {
    pub log_type: LogType,
    /// Name of the timestamp field (JSON / Logfmt only).
    pub timestamp_field: Option<String>,
    /// Name of the severity / level field (JSON / Logfmt only).
    pub level_field: Option<String>,
    /// Name of the primary message field (JSON / Logfmt only).
    pub message_field: Option<String>,
    /// Detected strftime-style timestamp format, e.g. `"%Y-%m-%dT%H:%M:%S"`.
    pub timestamp_format: Option<String>,
    /// True when consecutive lines appear to form single logical events.
    pub multiline: bool,
}

/// Quantitative statistics derived from the raw sample text.
///
/// All counts are `i64` rather than `u64` so they round-trip through BSON
/// without overflow (BSON's integer ceiling is `i64::MAX`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleStats {
    pub total_lines: i64,
    pub non_empty_lines: i64,
    pub empty_line_ratio: f64,
    pub avg_line_length: f64,
    /// Wall-clock span in seconds inferred from the first and last timestamp in
    /// the sample. `None` when no timestamps could be parsed.
    pub time_span_secs: Option<i64>,
    /// Counts per normalised severity label (`"info"`, `"error"`, …).
    pub level_distribution: HashMap<String, i64>,
    /// Ratio of unique lines to total lines — a measure of repetition.
    pub unique_line_ratio: f64,
}

/// Result of scanning the sample for agentic / LLM activity signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticScan {
    /// Fraction of lines that matched at least one agentic pattern (0.0 – 1.0).
    pub signal_score: f64,
    /// True when `signal_score` meets or exceeds the configured threshold.
    pub worth_classifying: bool,
    /// Framework names detected in the sample (e.g. `"langchain"`, `"openai"`).
    pub detected_frameworks: Vec<String>,
    /// Human-readable pattern labels that produced a hit.
    pub matched_patterns: Vec<String>,
    /// Number of lines that triggered at least one agentic pattern.
    pub agentic_line_count: i64,
}

/// Inferred type for a field observed inside structured log lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Number,
    Bool,
    Object,
    Array,
    Null,
}

/// Statistics about one field observed across multiple structured log lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub inferred_type: FieldType,
    /// Fraction of sampled lines in which this field was present (0.0 – 1.0).
    pub presence_ratio: f64,
    /// True when the field looks like a unique identifier (UUID, hash, …).
    pub is_identifier: bool,
}

/// Lightweight schema inferred from a structured (JSON or Logfmt) sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSchema {
    pub fields: Vec<FieldInfo>,
    /// Fraction of lines from which field information could be extracted.
    pub sample_coverage: f64,
}

/// Which LLM prompt template best fits this sample's structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplate {
    JsonAgent,
    LogfmtAgent,
    Syslog,
    Generic,
}

impl PromptTemplate {
    pub fn as_str(&self) -> &'static str {
        match self {
            PromptTemplate::JsonAgent => "json_agent",
            PromptTemplate::LogfmtAgent => "logfmt_agent",
            PromptTemplate::Syslog => "syslog",
            PromptTemplate::Generic => "generic",
        }
    }
}

/// Hints produced by preprocessing to guide downstream LLM ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionHints {
    pub prompt_template: PromptTemplate,
    /// Suggested number of log lines to include in a single LLM prompt chunk.
    pub suggested_chunk_size: i32,
    /// Whether the sample is worth sending to the LLM classifier at all.
    pub worth_classifying: bool,
    /// Human-readable explanation when `worth_classifying` is false.
    pub skip_reason: Option<String>,
    /// Processing priority (higher = more urgent); derived from signal score.
    pub priority: i32,
}

/// Lifecycle state of the LLM classification for this sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    Pending,
    Classified,
    Skipped,
    Failed,
}

impl ClassificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassificationStatus::Pending => "pending",
            ClassificationStatus::Classified => "classified",
            ClassificationStatus::Skipped => "skipped",
            ClassificationStatus::Failed => "failed",
        }
    }
}

// ─── UpsideGate entity model ──────────────────────────────────────────────────

/// Typed category for a parsed log event entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    PromptEvent,
    CompletionEvent,
    ToolCallEvent,
    ToolResultEvent,
    RetrievalEvent,
    AgentStep,
    McpEvent,
    ContextWindow,
    #[default]
    Unknown,
}

/// Semantic role of an entity within an AI execution — replaces traditional
/// log severity (info / warning / error) for AI-native logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRole {
    SystemPrompt,
    UserTurn,
    AssistantTurn,
    ToolInvocation,
    ToolResponse,
    RetrievalQuery,
    RetrievalResult,
    AgentReasoning,
    AgentAction,
    AgentObservation,
    McpRequest,
    McpResponse,
    ContextAssembly,
    MemoryRead,
    MemoryWrite,
    #[default]
    Unknown,
}

/// How a [`RelationEdge`] was established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelationSource {
    /// Derived from an explicit field in the log (e.g. a matching `tool_call_id`).
    Explicit,
    /// Derived from structural position and type-pair rules.
    #[default]
    Inferred,
    /// Directly stated in a structured metadata field.
    Parsed,
}

/// Directed relationship type between two [`EntityRecord`] instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationType {
    /// A prompt or agent step caused a tool call or downstream event.
    TriggeredBy,
    /// An activity produced this entity (LLM → completion, tool call → result).
    Generated,
    /// A retrieval result informed the construction of a prompt.
    Informed,
    /// Temporal successor in the same agent run.
    FollowedBy,
    /// A completion directly responded to a prompt.
    RespondedTo,
    /// A context window was assembled from this entity.
    AssembledFrom,
    /// Entity belongs to a containing trace / session / run.
    PartOf,
    /// An agent step delegated work to an MCP server.
    DelegatedTo,
}

/// A single parsed log event, typed and enriched by UpsideGate stages 6–7.
///
/// Stored **only** inside [`SampleMetadata::entities`] — there is no separate
/// entity collection. `GraphWriter` persists relations, PROV triples and spans
/// to their own collections, but entities stay embedded in the owning
/// `sample_metadata` document.
///
/// That is why looking one up by id
/// ([`MongoRepository::fetch_entity_by_id`]) queries `sample_metadata` with a
/// positional projection rather than a collection scan, and why
/// `entities.entity_id` carries a multikey index.
///
/// [`MongoRepository::fetch_entity_by_id`]: crate::repository::MongoRepository::fetch_entity_by_id
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    /// Stable content-derived identifier (32-hex-char SHA-256 of
    /// `(sample_hash, "entity", line_index, raw_text)`, see
    /// [`crate::preprocessing::ids::derive_entity_id`]).  Same content ⇒
    /// same id, so the graph writer's upsert filters actually match on
    /// re-runs instead of inserting duplicates.
    pub entity_id: String,
    pub entity_type: EntityType,
    pub semantic_role: SemanticRole,
    /// FK to the parent [`SampleRecord`] / [`SampleMetadata`].
    pub sample_hash: String,
    pub target_id: String,
    /// W3C-OTel 16-byte hex trace id, derived from `sample_hash` so all
    /// entities in a sample share the same trace and re-runs reuse it.
    /// See [`crate::preprocessing::ids::derive_trace_id`].
    pub trace_id: String,
    /// W3C-OTel 8-byte hex span id, derived from `(sample_hash, line_index)`.
    /// See [`crate::preprocessing::ids::derive_span_id`].
    pub span_id: String,
    /// Span id of the logical parent entity, if any.
    pub parent_span_id: Option<String>,
    /// W3C PROV entity URI: `ug:entity:{entity_id}`
    pub prov_entity_id: String,
    /// W3C PROV activity URI: `ug:activity:{sample_hash}:{line_index}`
    pub prov_activity_id: String,
    /// Zero-based line position within the sample content.
    pub line_index: u32,
    /// The original log line(s) that produced this entity.
    pub raw_text: String,
    /// Key/value fields extracted from the log line (JSON / Logfmt only).
    pub extracted_fields: HashMap<String, serde_json::Value>,
    /// LLM model identifier detected in the log (e.g. `"gpt-4o"`).
    pub model_id: Option<String>,
    /// Tool name for `ToolCallEvent` / `ToolResultEvent`.
    pub tool_name: Option<String>,
    /// MCP server identifier for `McpEvent`.
    pub mcp_server_id: Option<String>,
    /// Token count from `prompt_tokens` / `completion_tokens` fields.
    pub token_count: Option<u32>,
    /// Latency in milliseconds extracted from the log.
    pub latency_ms: Option<u64>,
    /// Timestamp parsed from the log line itself.
    pub timestamp_utc: Option<DateTime>,
    /// FK to the content embedding record — populated asynchronously by the
    /// embedding worker (Phase 5).
    pub content_embedding_id: Option<String>,
    /// FK to the behavioral embedding record — populated asynchronously.
    pub behavioral_embedding_id: Option<String>,
    /// Task this entity belongs to (Stage 11).
    ///
    /// Unlike every other id here, this one is **not** scoped to the sample: two
    /// samples carrying the same `session_id` share a `task_id`, which is how a
    /// task spanning several samples reassembles. Empty on documents written
    /// before Stage 11, and when `TASK_CORRELATION_ENABLED=false`.
    ///
    /// See [`crate::preprocessing::task_correlator`].
    #[serde(default)]
    pub task_id: String,
    /// The raw correlation value `task_id` was derived from, for display.
    /// `None` when the task fell back to sample scope.
    #[serde(default)]
    pub correlation_key: Option<String>,
}

/// A directed relationship between two [`EntityRecord`] instances.
///
/// Stored inside [`SampleMetadata::relations`]; also written to the
/// `entity_edges` collection when the graph writer is enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEdge {
    /// Stable content-derived identifier (32-hex-char SHA-256 of
    /// `(sample_hash, "rel", relation_type, source_entity_id, target_entity_id)`,
    /// see [`crate::preprocessing::ids::derive_relation_id`]).  Re-running
    /// the relation extractor produces the same id, making the graph
    /// writer's upsert path idempotent.
    pub relation_id: String,
    pub relation_type: RelationType,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub sample_hash: String,
    /// `1.0` = explicit field match; `0.7` = positional inference; etc.
    pub confidence: f32,
    pub source: RelationSource,
    pub created_at: DateTime,
}

// ─── TaskRecord ───────────────────────────────────────────────────────────────

/// One task — the index of the task-scoped semantic database.
///
/// Stored in the `tasks` collection, keyed on `task_id`. Accumulates rather than
/// being replaced: each sample that belongs to a task `$addToSet`s itself into
/// `sample_hashes` and `trace_ids`, so a task spanning several log samples is
/// stitched together as they arrive, in any order.
///
/// This is what makes "find similar tasks, then open their graph" possible:
/// `sample_hashes` is the join key back to `sample_metadata`, `entity_edges` and
/// `otel_spans` for the full interaction graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Stable 32-hex-char id, shared by every sample in this task.
    pub task_id: String,
    /// Which field the id was derived from — `session_id`, `run_id`, … or
    /// `sample` for a fallback boundary.
    pub task_id_source: String,
    /// The raw correlation value, for display. `None` for a sample fallback.
    #[serde(default)]
    pub correlation_key: Option<String>,
    /// Every sample observed as part of this task. The join key to the graph.
    #[serde(default)]
    pub sample_hashes: Vec<String>,
    /// Every OTel trace making up this task — one per sample, since `trace_id`
    /// remains sample-scoped by design.
    #[serde(default)]
    pub trace_ids: Vec<String>,
    /// Sampling targets this task's data came from. More than one means the task
    /// was observed across several sources.
    #[serde(default)]
    pub target_ids: Vec<String>,
    /// Running totals across all the task's samples.
    #[serde(default)]
    pub entity_count: u32,
    #[serde(default)]
    pub relation_count: u32,
    /// When this task was first and most recently seen. `last_seen` advances as
    /// further samples arrive; `first_seen` is set once on insert.
    pub first_seen: DateTime,
    pub last_seen: DateTime,
    /// The task's goal statement, for semantic search. Populated by Phase 3 —
    /// always `None` today.
    #[serde(default)]
    pub intent_text: Option<String>,
}

// ─── SampleMetadata ───────────────────────────────────────────────────────────

/// Full preprocessing result stored in the `sample_metadata` collection.
///
/// Keyed by `sample_hash` — the same hash used in `SampleRecord` — so the two
/// documents can always be joined without a secondary index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleMetadata {
    pub sample_hash: String,
    pub target_id: String,
    pub analyzed_at: DateTime,
    /// Monotonically incremented string version so the pipeline can re-process
    /// old records when the preprocessor logic changes. Format: `"1"`, `"2"`, …
    pub preprocessing_version: String,
    pub format: LogFormat,
    pub stats: SampleStats,
    pub agentic_scan: AgenticScan,
    /// Only present for structured (JSON / Logfmt) samples.
    pub schema: Option<LogSchema>,
    pub ingestion_hints: IngestionHints,
    pub classification_status: ClassificationStatus,
    // ── UpsideGate fields (Phase 0+) ─────────────────────────────────────────
    /// OTel-compatible 32-hex-char trace id generated at pipeline entry.
    /// Links all entity records for this sample to a single trace.
    /// Absent on v1 documents — use `#[serde(default)]` for backward compat.
    #[serde(default)]
    pub otel_trace_id: String,
    /// Typed entity records extracted from the log sample (Stage 6).
    /// Populated by `preprocessing::entity_extractor`; may be empty when no
    /// agentic patterns match the content.
    #[serde(default)]
    pub entities: Vec<EntityRecord>,
    /// Directed relation edges between entities (Stage 8).
    #[serde(default)]
    pub relations: Vec<RelationEdge>,
    /// Snapshot counts — redundant with `entities.len()` / `relations.len()`
    /// but useful for MongoDB aggregation queries without array unwinding.
    #[serde(default)]
    pub entity_count: u32,
    #[serde(default)]
    pub relation_count: u32,
    /// Task this sample belongs to (Stage 11).
    ///
    /// Shared with any other sample carrying the same correlation key, so this is
    /// the join key for "the whole graph for this task". Empty on pre-Stage-11
    /// documents and when `TASK_CORRELATION_ENABLED=false`.
    #[serde(default)]
    pub task_id: String,
    /// Which field `task_id` came from — `session_id`, `run_id`, … or `sample`
    /// when the log carried no correlation key.
    ///
    /// Recorded so an audit does not overstate its own confidence: a boundary
    /// derived from `session_id` is meaningful; one derived from the sample hash
    /// only means "we could not tell".
    #[serde(default)]
    pub task_id_source: String,
}

impl SampleMetadata {
    /// Serialise to a BSON `Document` suitable for `insert_one` / `replace_one`.
    ///
    /// We rely on `bson::to_document` here because all numeric fields in this
    /// type use `i64` / `i32` / `f64`, so there is no overflow risk.
    pub fn to_document(&self) -> Result<Document, AppError> {
        bson::to_document(self).map_err(|error| {
            AppError::Validation(format!("failed to serialise SampleMetadata: {error}"))
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────

fn take_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_non_empty(values: &[Option<&str>]) -> Option<String> {
    values.iter().find_map(|value| take_string(*value))
}

fn determine_auth_method(raw: &RawTargetDocument) -> Result<AuthMethod, String> {
    let credentials = raw.credentials.as_ref();
    // If the document omits `auth_method`, infer it from the credential material so
    // existing records do not have to be rewritten just to satisfy this service.
    let explicit = credentials
        .and_then(|value| value.auth_method.as_deref())
        .or_else(|| {
            if raw.password.is_some()
                || credentials
                    .and_then(|value| value.password.as_ref())
                    .is_some()
            {
                Some("password")
            } else if raw.private_key.is_some()
                || raw.private_key_path.is_some()
                || credentials
                    .and_then(|value| value.private_key.as_ref())
                    .is_some()
                || credentials
                    .and_then(|value| value.private_key_path.as_ref())
                    .is_some()
            {
                Some("private_key")
            } else {
                None
            }
        })
        .map(|value| value.trim().to_ascii_lowercase());

    match explicit.as_deref() {
        Some("password") => {
            let password = first_non_empty(&[
                raw.password.as_deref(),
                credentials.and_then(|value| value.password.as_deref()),
            ])
            .ok_or_else(|| "password auth selected but no password was provided".to_string())?;
            Ok(AuthMethod::Password { password })
        }
        Some("private_key") | Some("key") => {
            if let Some(private_key) = first_non_empty(&[
                raw.private_key.as_deref(),
                credentials.and_then(|value| value.private_key.as_deref()),
            ]) {
                Ok(AuthMethod::PrivateKeyInline {
                    private_key,
                    passphrase: first_non_empty(&[
                        raw.private_key_passphrase.as_deref(),
                        credentials.and_then(|value| value.passphrase.as_deref()),
                    ]),
                })
            } else if let Some(private_key_path) = first_non_empty(&[
                raw.private_key_path.as_deref(),
                credentials.and_then(|value| value.private_key_path.as_deref()),
            ]) {
                Ok(AuthMethod::PrivateKeyPath {
                    private_key_path,
                    passphrase: first_non_empty(&[
                        raw.private_key_passphrase.as_deref(),
                        credentials.and_then(|value| value.passphrase.as_deref()),
                    ]),
                })
            } else {
                Err("private_key auth selected but no key material was provided".to_string())
            }
        }
        Some(other) => Err(format!("unsupported auth_method `{other}`")),
        // No credentials present — allow the connection attempt to proceed
        // without them; the SSH inspector will try agent auth then "none".
        None => Ok(AuthMethod::None),
    }
}

fn is_valid_collection_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\0')
        && !value.contains('$')
        && !value.starts_with("system.")
}

// ─── LLM Classification ───────────────────────────────────────────────────────

/// Severity level assigned by the LLM classifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
    Normal,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning  => "warning",
            Severity::Info     => "info",
            Severity::Normal   => "normal",
        }
    }

    /// Numeric priority for threshold comparisons (higher = more severe).
    pub fn level(&self) -> u8 {
        match self {
            Severity::Normal   => 0,
            Severity::Info     => 1,
            Severity::Warning  => 2,
            Severity::Critical => 3,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" => Severity::Critical,
            "warning"  => Severity::Warning,
            "info"     => Severity::Info,
            _          => Severity::Normal,
        }
    }
}

/// A single notable pattern extracted from the log sample by the classifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Short label for the pattern, e.g. "repeated auth failure".
    pub pattern:  String,
    /// Number of occurrences in the sample window.
    pub count:    u32,
    /// Per-finding severity ("critical", "warning", "info").
    pub severity: String,
    /// One verbatim log line that best illustrates the pattern.
    pub example:  String,
}

/// Full LLM classification result stored in the `classifications` collection.
///
/// Keyed by `sample_hash` — same key as `SampleRecord` and `SampleMetadata` —
/// so all three documents can be joined without a secondary index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRecord {
    pub sample_hash:   String,
    pub target_id:     String,
    pub classified_at: DateTime,
    /// Model string used, e.g. `"claude-haiku-4-5-20251001"`.
    pub model:         String,
    pub severity:      Severity,
    /// Broad category tags, e.g. `["error", "anomaly", "performance"]`.
    pub categories:    Vec<String>,
    /// 1–3 sentence human-readable summary of what the classifier found.
    pub summary:       String,
    pub key_findings:  Vec<Finding>,
    pub recommendations: Vec<String>,
    /// Self-reported confidence from the model (0.0 – 1.0).
    pub confidence:    f64,
    pub input_tokens:  u32,
    pub output_tokens: u32,
    /// Monotonically incremented version string for re-classification tracking.
    pub classification_version: String,
}

impl ClassificationRecord {
    pub fn to_document(&self) -> Result<Document, AppError> {
        bson::to_document(self).map_err(|error| {
            AppError::Validation(format!(
                "failed to serialise ClassificationRecord: {error}"
            ))
        })
    }
}
