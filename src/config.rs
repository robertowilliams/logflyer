use std::env;
use std::fmt;
use std::path::PathBuf;

use crate::error::ConfigError;
use crate::sampling::SamplingMode;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub mongo: MongoConfig,
    pub sampling: SamplingConfig,
    pub service: ServiceConfig,
    pub discovery: DiscoveryConfig,
    pub logging: LoggingConfig,
    pub preprocessing: PreprocessingConfig,
}

#[derive(Debug, Clone)]
pub struct PreprocessingConfig {
    /// Run the preprocessing pipeline after each new sample is stored.
    pub enabled: bool,
    /// Minimum fraction of lines that must match an agentic pattern before the
    /// sample is flagged as worth classifying (0.0 – 1.0).
    pub agentic_threshold: f64,
    /// Maximum number of lines examined when extracting the log schema.
    pub max_schema_lines: usize,
    /// TCP port for the Prometheus `/metrics` HTTP listener.
    /// Set to 0 to disable the listener entirely.
    pub metrics_port: u16,
}

#[derive(Debug, Clone)]
pub struct MongoConfig {
    pub uri: String,
    pub source_db_name: String,
    pub source_collection_name: String,
    pub destination_db_name: String,
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

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        // The service uses `.env` as its single configuration surface so deployments can
        // promote the same binary through environments without rebuilding.
        Ok(Self {
            mongo: MongoConfig {
                uri: required("MONGODB_URI")?,
                source_db_name: with_default("SOURCE_DB_NAME", "vectadb"),
                source_collection_name: with_default("SOURCE_COLLECTION_NAME", "ai_targets"),
                destination_db_name: with_default("DESTINATION_DB_NAME", "log_samples"),
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
            preprocessing: PreprocessingConfig {
                enabled: bool_flag("PREPROCESSING_ENABLED", true),
                agentic_threshold: positive_f64("PREPROCESSING_AGENTIC_THRESHOLD", 0.02)?,
                max_schema_lines: positive_usize("PREPROCESSING_MAX_SCHEMA_LINES", 200)?,
                metrics_port: optional_u16("METRICS_PORT", 9090)?,
            },
        })
    }
}

fn required(name: &str) -> Result<String, ConfigError> {
    env::var(name).map_err(|_| ConfigError::MissingVar(name.to_string()))
}

fn with_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
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

/// Public re-export so `main.rs` can read one-off boolean env vars without
/// duplicating the parsing logic.
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

/// Parse a u16 env var.  Returns the default when the variable is unset.
/// 0 is a valid value here (it disables the listener).
fn optional_u16(name: &str, default: u16) -> Result<u16, ConfigError> {
    match env::var(name) {
        Err(_) => Ok(default),
        Ok(raw) => raw
            .parse::<u16>()
            .map_err(|_| ConfigError::InvalidVar(name.to_string(), raw)),
    }
}

fn parse_patterns(value: &str) -> Vec<String> {
    // Discovery patterns are configured as a comma-separated list so operators can
    // widen or narrow the search without touching code.
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}
