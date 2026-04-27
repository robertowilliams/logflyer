use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use file_rotate::{compression::Compression, suffix::AppendCount, ContentLimit, FileRotate};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::LoggingConfig;
use crate::error::AppError;

#[derive(Clone)]
struct RotatingWriter {
    inner: Arc<Mutex<FileRotate<AppendCount>>>,
}

impl RotatingWriter {
    fn new(path: PathBuf, max_files: usize, max_file_size_bytes: usize) -> Self {
        let file_rotate = FileRotate::new(
            path,
            AppendCount::new(max_files),
            ContentLimit::Bytes(max_file_size_bytes),
            Compression::None,
            None,
        );

        Self {
            inner: Arc::new(Mutex::new(file_rotate)),
        }
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "log writer lock poisoned"))?;
        guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "log writer lock poisoned"))?;
        guard.flush()
    }
}

pub fn init_logging(config: &LoggingConfig) -> Result<WorkerGuard, AppError> {
    fs::create_dir_all(&config.directory)?;

    let file_path = config
        .directory
        .join(format!("{}.log", config.file_base_name));

    // The rotating writer enforces size-based rollover so long-running services do not
    // accumulate unbounded local log files.
    let rotating_writer =
        RotatingWriter::new(file_path, config.max_files, config.max_file_size_bytes);
    let (non_blocking_writer, guard) = tracing_appender::non_blocking(rotating_writer);

    let filter = EnvFilter::try_new(config.level.clone())
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|error| {
            AppError::Config(crate::error::ConfigError::InvalidVar(
                "LOG_LEVEL".to_string(),
                error.to_string(),
            ))
        })?;

    let stdout_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(std::io::stdout);

    // Writing JSON to both stdout and file keeps logs friendly for containers while also
    // leaving behind rotated local files for incident review.
    let file_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(non_blocking_writer);

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .map_err(|error| {
            AppError::Ssh(format!("failed to initialize logging subscriber: {error}"))
        })?;

    Ok(guard)
}
