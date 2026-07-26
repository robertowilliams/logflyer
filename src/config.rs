use std::env;
use std::fmt;
use std::path::PathBuf;

use crate::error::ConfigError;
use crate::models::Severity;
use crate::sampling::SamplingMode;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub mongo: MongoConfig,
    pub sampling: SamplingConfig,
    pub service: ServiceConfig,
    pub discovery: DiscoveryConfig,
    pub logging: LoggingConfig,
    pub preprocessing: PreprocessingConfig,
    pub classification: ClassificationConfig,
    pub embedding: EmbeddingConfig,
    pub output: OutputConfig,
    pub notification: NotificationConfig,
    pub config_history: ConfigHistoryConfig,
}

#[derive(Debug, Clone)]
pub struct PreprocessingConfig {
    pub enabled: bool,
    pub agentic_threshold: f64,
    pub max_schema_lines: usize,
    pub metrics_port: u16,
    /// Master switch for stages 6–10 (entity extraction, semantic
    /// classification, relation extraction, PROV linking, OTel building).
    /// When `false` the upstream stages 1–5 still run, but `entities`,
    /// `relations`, prov triples, and OTel spans are all empty.
    pub entity_extraction_enabled: bool,
    /// Skip the async output adapters (graph + vector writers) when fewer
    /// than this many entities are extracted from a sample.  Setting to 0
    /// disables the gate and writes for every sample with ≥1 entity.
    pub min_entities_for_persist: usize,
    /// Switch for stage 11 (task correlation).  When `false`, `task_id` and
    /// `task_id_source` stay empty and nothing is written to the `tasks`
    /// collection — exactly the behaviour before stage 11 existed.
    ///
    /// Off by default: task correlation changes nothing about existing fields,
    /// but it does start writing a new collection, and that should be opted into.
    pub task_correlation_enabled: bool,
}

/// Configuration for the Phase 6 output adapters.
///
/// These are independent kill-switches so each can be rolled out separately.
/// `graph_writer_backend` and `vector_writer_backend` are reserved for future
/// Neo4j / Qdrant impls — only `"mongodb"` is wired today.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// When `true`, persist relation edges, PROV triples, and OTel spans to
    /// the destination MongoDB after each preprocessing run.
    pub graph_writer_enabled: bool,
    /// Reserved for future multi-backend support.  Today only `"mongodb"` is
    /// implemented; setting any other value falls back to the MongoDB writer
    /// with a warning at startup.
    pub graph_writer_backend: String,
    /// When `true`, persist content + behavioral embeddings to the vector
    /// collections.  Independent of `graph_writer_enabled`.
    pub vector_writer_enabled: bool,
    pub vector_writer_backend: String,
}

impl OutputConfig {
    pub fn from_env() -> Self {
        Self {
            graph_writer_enabled: bool_flag("GRAPH_WRITER_ENABLED", false),
            graph_writer_backend: with_default("GRAPH_WRITER_BACKEND", "mongodb"),
            vector_writer_enabled: bool_flag("VECTOR_WRITER_ENABLED", false),
            vector_writer_backend: with_default("VECTOR_WRITER_BACKEND", "mongodb"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MongoConfig {
    pub uri: String,
    pub source_db_name: String,
    pub source_collection_name: String,
    pub destination_db_name: String,
    pub tracking_db_name: String,
    pub tracking_collection_name: String,
}

#[derive(Debug, Clone)]
pub struct SamplingConfig {
    pub mode: SamplingMode,
    pub line_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Once,
    Periodic,
}

impl fmt::Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunMode::Once => write!(f, "once"),
            RunMode::Periodic => write!(f, "periodic"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub run_mode: RunMode,
    pub poll_interval_secs: u64,
    pub concurrency: usize,
    pub ssh_timeout_secs: u64,
    /// TCP port for the REST API server. Set to 0 to disable.
    pub api_port: u16,
}

#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub max_depth: usize,
    pub max_files_per_target: usize,
    pub find_patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub level: String,
    pub directory: PathBuf,
    pub file_base_name: String,
    pub max_file_size_bytes: usize,
    pub max_files: usize,
}

#[derive(Debug, Clone)]
pub struct ConfigHistoryConfig {
    pub enabled: bool,
    pub master_key: Option<String>,
    pub key_id: String,
    pub collection_name: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            mongo: MongoConfig {
                uri: required("MONGODB_URI")?,
                source_db_name: with_default("SOURCE_DB_NAME", "vectadb"),
                source_collection_name: with_default("SOURCE_COLLECTION_NAME", "ai_targets"),
                destination_db_name: with_default("DESTINATION_DB_NAME", "log_samples"),
                tracking_db_name: with_default("TRACKING_DB_NAME", "loggingtracker"),
                tracking_collection_name: with_default(
                    "TRACKING_COLLECTION_NAME",
                    "logging_tracks",
                ),
            },
            sampling: SamplingConfig {
                mode: parse_sampling_mode(&with_default("SAMPLE_MODE", "both"))?,
                line_count: positive_usize("SAMPLE_LINE_COUNT", 50)?,
            },
            service: ServiceConfig {
                run_mode: parse_run_mode(&with_default("RUN_MODE", "once"))?,
                poll_interval_secs: positive_u64("POLL_INTERVAL_SECS", 300)?,
                concurrency: positive_usize("CONCURRENCY", 4)?,
                ssh_timeout_secs: positive_u64("SSH_TIMEOUT_SECS", 15)?,
                api_port: optional_u16("API_PORT", 8080)?,
            },
            discovery: DiscoveryConfig {
                max_depth: positive_usize("REMOTE_MAX_DEPTH", 3)?,
                max_files_per_target: positive_usize("REMOTE_MAX_FILES_PER_TARGET", 100)?,
                find_patterns: parse_patterns(&with_default(
                    "REMOTE_FIND_PATTERNS",
                    "*.log,*.out,*.txt",
                )),
            },
            logging: LoggingConfig {
                level: with_default("LOG_LEVEL", "info"),
                directory: PathBuf::from(with_default("LOG_DIRECTORY", "./logs")),
                file_base_name: with_default("LOG_FILE_BASE_NAME", "logflayer"),
                max_file_size_bytes: positive_usize("LOG_MAX_FILE_SIZE_BYTES", 10 * 1024 * 1024)?,
                max_files: positive_usize("LOG_MAX_FILES", 10)?,
            },
            classification: ClassificationConfig::from_env(),
            embedding: EmbeddingConfig::from_env(),
            output: OutputConfig::from_env(),
            notification: NotificationConfig::from_env(),
            config_history: ConfigHistoryConfig::from_env()?,
            preprocessing: PreprocessingConfig {
                enabled: bool_flag("PREPROCESSING_ENABLED", true),
                agentic_threshold: positive_f64("PREPROCESSING_AGENTIC_THRESHOLD", 0.02)?,
                max_schema_lines: positive_usize("PREPROCESSING_MAX_SCHEMA_LINES", 200)?,
                metrics_port: optional_u16("METRICS_PORT", 9090)?,
                entity_extraction_enabled: bool_flag("ENTITY_EXTRACTION_ENABLED", true),
                min_entities_for_persist: env::var("ENTITY_EXTRACTION_MIN_ENTITIES")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1),
                task_correlation_enabled: bool_flag("TASK_CORRELATION_ENABLED", false),
            },
        })
    }
}

impl ConfigHistoryConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let enabled = bool_flag("CONFIG_HISTORY_ENABLED", false);
        let master_key = optional_non_empty("CONFIG_HISTORY_MASTER_KEY");

        if enabled && master_key.is_none() {
            return Err(ConfigError::MissingVar(
                "CONFIG_HISTORY_MASTER_KEY".to_string(),
            ));
        }

        Ok(Self {
            enabled,
            master_key,
            key_id: with_default("CONFIG_HISTORY_KEY_ID", "logflayer-config-v1"),
            collection_name: with_default(
                "CONFIG_HISTORY_COLLECTION_NAME",
                "app_settings_history",
            ),
        })
    }
}

fn required(name: &str) -> Result<String, ConfigError> {
    env::var(name).map_err(|_| ConfigError::MissingVar(name.to_string()))
}

fn with_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn optional_non_empty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn positive_usize(name: &str, default: usize) -> Result<usize, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| ConfigError::InvalidVar(name.to_string(), raw.clone()))?;
    if parsed == 0 {
        return Err(ConfigError::InvalidVar(name.to_string(), raw));
    }
    Ok(parsed)
}

fn positive_u64(name: &str, default: u64) -> Result<u64, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = raw
        .parse::<u64>()
        .map_err(|_| ConfigError::InvalidVar(name.to_string(), raw.clone()))?;
    if parsed == 0 {
        return Err(ConfigError::InvalidVar(name.to_string(), raw));
    }
    Ok(parsed)
}

fn parse_run_mode(value: &str) -> Result<RunMode, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "once" => Ok(RunMode::Once),
        "periodic" => Ok(RunMode::Periodic),
        other => Err(ConfigError::InvalidVar(
            "RUN_MODE".to_string(),
            other.to_string(),
        )),
    }
}

fn parse_sampling_mode(value: &str) -> Result<SamplingMode, ConfigError> {
    SamplingMode::from_env(value)
        .ok_or_else(|| ConfigError::InvalidVar("SAMPLE_MODE".to_string(), value.to_string()))
}

pub fn bool_flag_pub(name: &str, default: bool) -> bool {
    bool_flag(name, default)
}

fn bool_flag(name: &str, default: bool) -> bool {
    match env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "true" | "1" | "yes" => true,
        "false" | "0" | "no" => false,
        _ => default,
    }
}

fn positive_f64(name: &str, default: f64) -> Result<f64, ConfigError> {
    let raw = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = raw
        .parse::<f64>()
        .map_err(|_| ConfigError::InvalidVar(name.to_string(), raw.clone()))?;
    if parsed <= 0.0 || !parsed.is_finite() {
        return Err(ConfigError::InvalidVar(name.to_string(), raw));
    }
    Ok(parsed)
}

fn optional_u16(name: &str, default: u16) -> Result<u16, ConfigError> {
    match env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => raw
            .parse::<u16>()
            .map_err(|_| ConfigError::InvalidVar(name.to_string(), raw)),
    }
}

fn parse_patterns(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Clone)]
pub struct ClassificationConfig {
    /// Master switch — false by default so the feature is opt-in.
    pub enabled: bool,
    /// API key — Anthropic key or Bearer token depending on `api_format`.
    pub api_key: String,
    /// Model string, e.g. `"claude-haiku-4-5-20251001"` or `"gpt-4o-mini"`.
    pub model: String,
    /// Only classify samples whose `signal_score` ≥ this value.
    pub signal_threshold: f64,
    /// Hard cap on API calls per sampling cycle (cost guard).
    pub max_per_cycle: usize,
    /// Maximum tokens allowed in the model response.
    pub max_output_tokens: u32,
    /// Base URL for the LLM API.
    /// Anthropic default: empty (uses https://api.anthropic.com).
    /// OpenAI default:    empty (uses https://api.openai.com).
    /// Custom:            e.g. http://localhost:11434 for Ollama.
    pub api_base_url: String,
    /// Wire format: `"anthropic"` (default) or `"openai"`.
    /// Any OpenAI-compatible provider (OpenAI, OpenRouter, Groq, Ollama,
    /// LM Studio, Together AI, …) works with `"openai"`.
    pub api_format: String,
}

/// Configuration for the Phase 5 embedding worker.
///
/// Supports any OpenAI-compatible embeddings endpoint.  Set
/// `EMBEDDING_API_BASE_URL` to use a self-hosted model (e.g. Ollama).
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Master switch — `false` by default so the feature is opt-in.
    pub enabled: bool,
    /// Bearer token for the embeddings API.  Falls back to `OPENAI_API_KEY`.
    pub api_key: String,
    /// Base URL of the embeddings endpoint.  Defaults to `https://api.openai.com`.
    pub api_base_url: String,
    /// Model name sent in the request body.
    pub model: String,
    /// Maximum number of Unicode code points to send per embedding request.
    /// Roughly 4 chars/token → 8 000 chars ≈ 2 000 tokens (safe for 8 k-token models).
    pub max_text_chars: usize,
    /// Requested output dimensions.  `0` omits the field from the request body
    /// (uses the model's native dimensionality).
    pub dimensions: u32,
}

impl EmbeddingConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: bool_flag("EMBEDDING_ENABLED", false),
            api_key: env::var("EMBEDDING_API_KEY")
                .or_else(|_| env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
            api_base_url: env::var("EMBEDDING_API_BASE_URL").unwrap_or_default(),
            model: with_default("EMBEDDING_MODEL", "text-embedding-3-small"),
            max_text_chars: env::var("EMBEDDING_MAX_TEXT_CHARS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(8_000),
            dimensions: env::var("EMBEDDING_DIMENSIONS")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1536),
        }
    }
}

impl ClassificationConfig {
    pub fn from_env() -> Self {
        let enabled = bool_flag("CLASSIFICATION_ENABLED", false);
        Self {
            enabled,
            api_key:          env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            model:            with_default("CLASSIFICATION_MODEL", "claude-haiku-4-5-20251001"),
            signal_threshold: env::var("CLASSIFICATION_SIGNAL_THRESHOLD")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.05),
            max_per_cycle: env::var("CLASSIFICATION_MAX_PER_CYCLE")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(10),
            max_output_tokens: env::var("CLASSIFICATION_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1024),
            api_base_url: env::var("CLASSIFICATION_API_BASE_URL").unwrap_or_default(),
            api_format:   with_default("CLASSIFICATION_API_FORMAT", "anthropic"),
        }
    }
}

// ── Admin settings (MongoDB-persisted overrides) ──────────────────────────────

/// All user-configurable settings that can be persisted in MongoDB and applied
/// on top of the env-var baseline at startup. Every field is `Option<_>` so
/// only values explicitly saved by the admin UI are overridden.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AdminSettings {
    // ── MongoDB ───────────────────────────────────────────────────────────────
    pub mongodb_uri:                      Option<String>,
    pub source_db_name:                   Option<String>,
    pub source_collection_name:           Option<String>,
    pub destination_db_name:              Option<String>,
    pub tracking_db_name:                 Option<String>,
    pub tracking_collection_name:         Option<String>,
    // ── Sampling ──────────────────────────────────────────────────────────────
    pub sample_mode:                      Option<String>,
    pub sample_line_count:                Option<u64>,
    // ── Service ───────────────────────────────────────────────────────────────
    pub run_mode:                         Option<String>,
    pub poll_interval_secs:               Option<u64>,
    pub concurrency:                      Option<u64>,
    pub ssh_timeout_secs:                 Option<u64>,
    pub api_port:                         Option<u64>,
    // ── Discovery ─────────────────────────────────────────────────────────────
    pub remote_max_depth:                 Option<u64>,
    pub remote_max_files_per_target:      Option<u64>,
    pub remote_find_patterns:             Option<String>,
    // ── Preprocessing ─────────────────────────────────────────────────────────
    pub preprocessing_enabled:            Option<bool>,
    pub preprocessing_agentic_threshold:  Option<f64>,
    pub preprocessing_max_schema_lines:   Option<u64>,
    pub metrics_port:                     Option<u64>,
    pub entity_extraction_enabled:        Option<bool>,
    pub entity_extraction_min_entities:   Option<u64>,
    // ── Output adapters ───────────────────────────────────────────────────────
    pub graph_writer_enabled:             Option<bool>,
    pub graph_writer_backend:             Option<String>,
    pub vector_writer_enabled:            Option<bool>,
    pub vector_writer_backend:            Option<String>,
    // ── Classification ────────────────────────────────────────────────────────
    pub classification_enabled:           Option<bool>,
    pub anthropic_api_key:                Option<String>,
    pub classification_model:             Option<String>,
    pub classification_signal_threshold:  Option<f64>,
    pub classification_max_per_cycle:     Option<u64>,
    pub classification_max_output_tokens: Option<u64>,
    pub classification_api_base_url:      Option<String>,
    pub classification_api_format:        Option<String>,
    // ── Notifications ─────────────────────────────────────────────────────────
    pub notification_enabled:             Option<bool>,
    pub notification_severity_threshold:  Option<String>,
    pub slack_webhook_url:                Option<String>,
    pub webhook_url:                      Option<String>,
    pub webhook_secret:                   Option<String>,
    // ── Logging ───────────────────────────────────────────────────────────────
    pub log_level:                        Option<String>,
    pub log_directory:                    Option<String>,
    pub log_file_base_name:               Option<String>,
    pub log_max_file_size_bytes:          Option<u64>,
    pub log_max_files:                    Option<u64>,
    // ── Config history ────────────────────────────────────────────────────────
    pub config_history_enabled:           Option<bool>,
    /// Sensitive — returned as "***" when set, stored masked in DB.
    /// Never written to history snapshots.
    pub config_history_master_key:        Option<String>,
    pub config_history_key_id:            Option<String>,
    pub config_history_collection_name:   Option<String>,
}

impl AppConfig {
    /// Merge MongoDB-stored admin overrides on top of the env-var baseline.
    /// Only `Some(_)` fields in `s` are applied; `None` fields leave the
    /// existing value unchanged.
    pub fn apply_admin_settings(mut self, s: AdminSettings) -> Self {
        use crate::models::Severity;

        // ── MongoDB ───────────────────────────────────────────────────────────
        if let Some(v) = s.mongodb_uri              { if !v.is_empty() && v != "***" { self.mongo.uri = v; } }
        if let Some(v) = s.source_db_name           { if !v.is_empty() { self.mongo.source_db_name = v; } }
        if let Some(v) = s.source_collection_name   { if !v.is_empty() { self.mongo.source_collection_name = v; } }
        if let Some(v) = s.destination_db_name      { if !v.is_empty() { self.mongo.destination_db_name = v; } }
        if let Some(v) = s.tracking_db_name         { if !v.is_empty() { self.mongo.tracking_db_name = v; } }
        if let Some(v) = s.tracking_collection_name { if !v.is_empty() { self.mongo.tracking_collection_name = v; } }

        // ── Sampling ──────────────────────────────────────────────────────────
        if let Some(v) = s.sample_mode {
            if let Some(mode) = crate::sampling::SamplingMode::from_env(&v) {
                self.sampling.mode = mode;
            }
        }
        if let Some(v) = s.sample_line_count { if v > 0 { self.sampling.line_count = v as usize; } }

        // ── Service ───────────────────────────────────────────────────────────
        if let Some(v) = s.run_mode {
            self.service.run_mode = match v.trim().to_ascii_lowercase().as_str() {
                "periodic" => RunMode::Periodic,
                _          => RunMode::Once,
            };
        }
        if let Some(v) = s.poll_interval_secs { if v > 0 { self.service.poll_interval_secs = v; } }
        if let Some(v) = s.concurrency        { if v > 0 { self.service.concurrency = v as usize; } }
        if let Some(v) = s.ssh_timeout_secs   { if v > 0 { self.service.ssh_timeout_secs = v; } }
        if let Some(v) = s.api_port           { if v > 0 && v <= 65535 { self.service.api_port = v as u16; } }

        // ── Discovery ─────────────────────────────────────────────────────────
        if let Some(v) = s.remote_max_depth            { if v > 0 { self.discovery.max_depth = v as usize; } }
        if let Some(v) = s.remote_max_files_per_target { if v > 0 { self.discovery.max_files_per_target = v as usize; } }
        if let Some(v) = s.remote_find_patterns {
            self.discovery.find_patterns = v.split(',')
                .map(str::trim).filter(|s| !s.is_empty())
                .map(ToString::to_string).collect();
        }

        // ── Preprocessing ─────────────────────────────────────────────────────
        if let Some(v) = s.preprocessing_enabled           { self.preprocessing.enabled = v; }
        if let Some(v) = s.preprocessing_agentic_threshold {
            if v > 0.0 && v.is_finite() { self.preprocessing.agentic_threshold = v; }
        }
        if let Some(v) = s.preprocessing_max_schema_lines { if v > 0 { self.preprocessing.max_schema_lines = v as usize; } }
        if let Some(v) = s.metrics_port { if v > 0 && v <= 65535 { self.preprocessing.metrics_port = v as u16; } }
        if let Some(v) = s.entity_extraction_enabled      { self.preprocessing.entity_extraction_enabled = v; }
        if let Some(v) = s.entity_extraction_min_entities { self.preprocessing.min_entities_for_persist = v as usize; }

        // ── Output adapters ───────────────────────────────────────────────────
        if let Some(v) = s.graph_writer_enabled  { self.output.graph_writer_enabled  = v; }
        if let Some(v) = s.graph_writer_backend  { if !v.is_empty() { self.output.graph_writer_backend  = v; } }
        if let Some(v) = s.vector_writer_enabled { self.output.vector_writer_enabled = v; }
        if let Some(v) = s.vector_writer_backend { if !v.is_empty() { self.output.vector_writer_backend = v; } }

        // ── Classification ────────────────────────────────────────────────────
        if let Some(v) = s.classification_enabled           { self.classification.enabled = v; }
        if let Some(v) = s.anthropic_api_key                { if !v.is_empty() { self.classification.api_key = v; } }
        if let Some(v) = s.classification_model             { if !v.is_empty() { self.classification.model = v; } }
        if let Some(v) = s.classification_signal_threshold  { self.classification.signal_threshold = v; }
        if let Some(v) = s.classification_max_per_cycle     { if v > 0 { self.classification.max_per_cycle = v as usize; } }
        if let Some(v) = s.classification_max_output_tokens { if v > 0 { self.classification.max_output_tokens = v as u32; } }
        if let Some(v) = s.classification_api_base_url      { self.classification.api_base_url = v; }
        if let Some(v) = s.classification_api_format        { if !v.is_empty() { self.classification.api_format = v; } }

        // ── Notifications ─────────────────────────────────────────────────────
        if let Some(v) = s.notification_enabled            { self.notification.enabled = v; }
        if let Some(v) = s.notification_severity_threshold { self.notification.severity_threshold = Severity::from_str(&v); }
        if let Some(v) = s.slack_webhook_url {
            self.notification.slack_webhook_url = if v.is_empty() { None } else { Some(v) };
        }
        if let Some(v) = s.webhook_url {
            self.notification.webhook_url = if v.is_empty() { None } else { Some(v) };
        }
        if let Some(v) = s.webhook_secret {
            self.notification.webhook_secret = if v.is_empty() { None } else { Some(v) };
        }

        // ── Logging ───────────────────────────────────────────────────────────
        if let Some(v) = s.log_level            { if !v.is_empty() { self.logging.level = v; } }
        if let Some(v) = s.log_directory        { if !v.is_empty() { self.logging.directory = std::path::PathBuf::from(v); } }
        if let Some(v) = s.log_file_base_name   { if !v.is_empty() { self.logging.file_base_name = v; } }
        if let Some(v) = s.log_max_file_size_bytes { if v > 0 { self.logging.max_file_size_bytes = v as usize; } }
        if let Some(v) = s.log_max_files        { if v > 0 { self.logging.max_files = v as usize; } }

        // ── Config history ────────────────────────────────────────────────────
        if let Some(v) = s.config_history_enabled          { self.config_history.enabled = v; }
        if let Some(v) = s.config_history_master_key       { if !v.is_empty() { self.config_history.master_key = Some(v); } }
        if let Some(v) = s.config_history_key_id           { if !v.is_empty() { self.config_history.key_id = v; } }
        if let Some(v) = s.config_history_collection_name  { if !v.is_empty() { self.config_history.collection_name = v; } }

        self
    }
}

// ── Notification ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NotificationConfig {
    /// Master switch. Default: false.
    pub enabled: bool,
    /// Only fire notifications when severity >= this level.
    /// Default: Critical.
    pub severity_threshold: Severity,
    /// Slack incoming-webhook URL. Notifications are sent here when set.
    pub slack_webhook_url: Option<String>,
    /// Generic HTTP endpoint. A JSON payload is POSTed here when set.
    pub webhook_url: Option<String>,
    /// Optional shared secret for HMAC-SHA256 signing of webhook payloads.
    /// Sent as `X-Logflayer-Signature: sha256=<hex>`.
    pub webhook_secret: Option<String>,
}

impl NotificationConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: bool_flag("NOTIFICATION_ENABLED", false),
            severity_threshold: Severity::from_str(&with_default(
                "NOTIFICATION_SEVERITY_THRESHOLD",
                "critical",
            )),
            slack_webhook_url: env::var("SLACK_WEBHOOK_URL").ok().filter(|s| !s.is_empty()),
            webhook_url: env::var("WEBHOOK_URL").ok().filter(|s| !s.is_empty()),
            webhook_secret: env::var("WEBHOOK_SECRET").ok().filter(|s| !s.is_empty()),
        }
    }
}
