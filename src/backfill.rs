//! Backfill command — processes existing `SampleRecord` documents that have
//! no corresponding `SampleMetadata`.
//!
//! Invoked via `logflayer backfill [--batch-size N] [--dry-run]`.
//!
//! The command reads unprocessed samples from MongoDB in pages of `batch_size`,
//! runs the preprocessing pipeline on each one (using `spawn_blocking` to keep
//! the async runtime responsive), and writes the resulting `SampleMetadata`
//! back to MongoDB.  It respects the `concurrency` setting from the service
//! config so it does not overwhelm the database.
//!
//! `--dry-run` runs the pipeline and prints results without writing anything.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use tracing::{error, info, warn};

use crate::config::AppConfig;
use crate::error::AppError;
use crate::metrics;
use crate::preprocessing::Preprocessor;
use crate::repository::MongoRepository;

/// Options parsed from CLI args in `main.rs`.
#[derive(Debug, Clone)]
pub struct BackfillOptions {
    /// How many unprocessed samples to fetch per database page.
    pub batch_size: usize,
    /// Print what would happen without writing to MongoDB.
    pub dry_run: bool,
    /// Re-process samples whose `preprocessing_version` is older than the
    /// current binary's version.  Requires `config.preprocessing.enabled`.
    pub reprocess_stale: bool,
}

impl Default for BackfillOptions {
    fn default() -> Self {
        Self {
            batch_size: 100,
            dry_run: false,
            reprocess_stale: false,
        }
    }
}

/// Run the full backfill job and return a summary.
pub async fn run(config: AppConfig, opts: BackfillOptions) -> Result<BackfillSummary, AppError> {
    let repository = MongoRepository::connect(&config.mongo).await?;
    repository.ping().await?;

    let preprocessor = Arc::new(Preprocessor::new(config.preprocessing.clone()));
    let concurrency = config.service.concurrency;

    let counters = Arc::new(BackfillCounters::default());

    info!(
        batch_size = opts.batch_size,
        dry_run = opts.dry_run,
        reprocess_stale = opts.reprocess_stale,
        concurrency,
        "Starting backfill"
    );

    let started = Instant::now();

    // Page through unprocessed samples.  We loop until the repository returns
    // an empty page, which signals that we've caught up.
    loop {
        let samples = repository
            .fetch_unprocessed_samples(opts.batch_size)
            .await?;

        if samples.is_empty() {
            break;
        }

        let page_len = samples.len();
        info!("Processing page of {} sample(s)", page_len);

        let repository = repository.clone();
        let preprocessor = Arc::clone(&preprocessor);
        let counters = Arc::clone(&counters);
        let dry_run = opts.dry_run;

        stream::iter(samples)
            .for_each_concurrent(concurrency, |sample| {
                let repository = repository.clone();
                let preprocessor = Arc::clone(&preprocessor);
                let counters = Arc::clone(&counters);

                async move {
                    counters.attempted.fetch_add(1, Ordering::Relaxed);

                    let hash = sample.sample_hash.clone();
                    let target = sample.target_id.clone();
                    let content = sample.sample_content.clone();
                    let prep = Arc::clone(&preprocessor);

                    let pipeline_start = Instant::now();

                    let metadata = match tokio::task::spawn_blocking(move || {
                        prep.run(&hash, &target, &content)
                    })
                    .await
                    {
                        Ok(m) => m,
                        Err(e) => {
                            error!(
                                sample_hash = %sample.sample_hash,
                                error = ?e,
                                "preprocessing task panicked"
                            );
                            counters.failed.fetch_add(1, Ordering::Relaxed);
                            metrics::record_error();
                            return;
                        }
                    };

                    metrics::record_duration(pipeline_start.elapsed().as_secs_f64());

                    let worth = metadata.ingestion_hints.worth_classifying;
                    let format = metadata.format.log_type.as_str();

                    if dry_run {
                        info!(
                            sample_hash = %metadata.sample_hash,
                            target_id  = %metadata.target_id,
                            log_type   = format,
                            worth_classifying = worth,
                            signal_score = %format!("{:.4}", metadata.agentic_scan.signal_score),
                            "[dry-run] would write metadata"
                        );
                        counters.written.fetch_add(1, Ordering::Relaxed);
                        return;
                    }

                    match repository.store_metadata(&metadata).await {
                        Ok(()) => {
                            counters.written.fetch_add(1, Ordering::Relaxed);
                            metrics::record_processed(worth);
                            if worth {
                                counters.agentic.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            warn!(
                                sample_hash = %metadata.sample_hash,
                                error = %e,
                                "failed to store metadata"
                            );
                            counters.failed.fetch_add(1, Ordering::Relaxed);
                            metrics::record_error();
                        }
                    }
                }
            })
            .await;

        // If the page was smaller than the batch size, we've reached the end.
        if page_len < opts.batch_size {
            break;
        }
    }

    let elapsed = started.elapsed();
    let summary = BackfillSummary {
        attempted: counters.attempted.load(Ordering::Relaxed),
        written: counters.written.load(Ordering::Relaxed),
        failed: counters.failed.load(Ordering::Relaxed),
        agentic: counters.agentic.load(Ordering::Relaxed),
        elapsed_secs: elapsed.as_secs_f64(),
        dry_run: opts.dry_run,
    };

    info!(
        attempted  = summary.attempted,
        written    = summary.written,
        failed     = summary.failed,
        agentic    = summary.agentic,
        elapsed_s  = %format!("{:.2}", summary.elapsed_secs),
        dry_run    = summary.dry_run,
        "Backfill complete"
    );

    Ok(summary)
}

// ─── Version-aware stale-metadata detection ───────────────────────────────────

/// Check MongoDB for `SampleMetadata` documents whose `preprocessing_version`
/// is older than `current_version` and delete them so the backfill loop will
/// re-process those samples.
///
/// Only called when `PREPROCESSING_REPROCESS_ON_VERSION_CHANGE=true`.
pub async fn purge_stale_metadata(
    repository: &MongoRepository,
    current_version: &str,
) -> Result<u64, AppError> {
    let purged = repository
        .delete_stale_metadata(current_version)
        .await?;

    if purged > 0 {
        info!(
            purged,
            current_version,
            "Purged stale metadata documents for reprocessing"
        );
    } else {
        info!("No stale metadata found — all documents are at version {}", current_version);
    }

    Ok(purged)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

#[derive(Default)]
struct BackfillCounters {
    attempted: AtomicU64,
    written:   AtomicU64,
    failed:    AtomicU64,
    agentic:   AtomicU64,
}

/// Summary returned to the caller (and printed by `main`).
#[derive(Debug)]
pub struct BackfillSummary {
    pub attempted:     u64,
    pub written:       u64,
    pub failed:        u64,
    pub agentic:       u64,
    pub elapsed_secs:  f64,
    pub dry_run:       bool,
}
