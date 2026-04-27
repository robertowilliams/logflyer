use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use mongodb::bson::DateTime;
use tracing::{error, info, warn};

use crate::config::{AppConfig, PreprocessingConfig, RunMode};
use crate::error::AppError;
use crate::metrics;
use crate::models::{RawTargetDocument, SampleRecord, ValidatedTarget};
use crate::preprocessing::Preprocessor;
use crate::repository::{MongoRepository, StoreOutcome};
use crate::ssh::SshLogInspector;
use crate::utils::compute_sample_hash;

#[derive(Clone)]
pub struct Application {
    config: AppConfig,
    repository: MongoRepository,
    inspector: Arc<SshLogInspector>,
}

impl Application {
    pub async fn build(config: AppConfig) -> Result<Self, AppError> {
        let repository = MongoRepository::connect(&config.mongo).await?;
        repository.ping().await?;

        let inspector = Arc::new(SshLogInspector::new(
            config.sampling.clone(),
            config.discovery.clone(),
            Duration::from_secs(config.service.ssh_timeout_secs),
        ));

        Ok(Self {
            config,
            repository,
            inspector,
        })
    }

    pub async fn run(&self) -> Result<(), AppError> {
        match self.config.service.run_mode {
            RunMode::Once => self.run_cycle().await,
            RunMode::Periodic => self.run_periodic().await,
        }
    }

    async fn run_periodic(&self) -> Result<(), AppError> {
        let mut interval =
            tokio::time::interval(Duration::from_secs(self.config.service.poll_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received shutdown signal, exiting periodic loop");
                    return Ok(());
                }
                _ = interval.tick() => {
                    if let Err(error) = self.run_cycle().await {
                        error!(error = %error, "sampling cycle finished with errors");
                    }
                }
            }
        }
    }

    async fn run_cycle(&self) -> Result<(), AppError> {
        let documents = self.repository.fetch_active_targets().await?;
        info!(active_targets = documents.len(), "starting sampling cycle");

        let repository = self.repository.clone();
        let inspector = Arc::clone(&self.inspector);

        // Each target runs in isolation under the configured concurrency limit so one
        // bad host or bad document cannot halt the rest of the batch.
        let preprocessing_config = self.config.preprocessing.clone();

        stream::iter(documents)
            .for_each_concurrent(self.config.service.concurrency, move |document| {
                let repository = repository.clone();
                let inspector = Arc::clone(&inspector);
                let preprocessing_config = preprocessing_config.clone();

                async move {
                    if let Err(error) =
                        process_document(repository, inspector, preprocessing_config, document)
                            .await
                    {
                        error!(error = %error, "target processing failed");
                    }
                }
            })
            .await;

        info!("sampling cycle completed");
        Ok(())
    }
}

async fn process_document(
    repository: MongoRepository,
    inspector: Arc<SshLogInspector>,
    preprocessing_config: PreprocessingConfig,
    document: mongodb::bson::Document,
) -> Result<(), AppError> {
    let raw = match RawTargetDocument::from_document(document) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(error = %error, "skipping malformed target document");
            return Ok(());
        }
    };

    let target = match ValidatedTarget::validate(raw.clone()) {
        Ok(target) => target,
        Err(errors) => {
            // Validation failures are intentionally non-fatal. The service logs the skip
            // reason and keeps moving so malformed records do not starve valid targets.
            warn!(
                document_id = %raw.document_id(),
                reasons = %errors.join("; "),
                "skipping invalid target document"
            );
            return Ok(());
        }
    };

    info!(
        target_id = %target.target_id,
        host = %target.host,
        directories = target.log_paths.len(),
        "processing target"
    );

    let drafts = inspector.collect_samples(target.clone()).await?;

    for draft in drafts {
        // The hash is computed before insertion so the repository can lean on a unique
        // index instead of custom read-before-write logic.
        let sample = SampleRecord {
            timestamp: DateTime::now(),
            sample_hash: compute_sample_hash(&draft),
            target_id: draft.target_id.clone(),
            source_file: draft.source_file.clone(),
            sample_content: draft.sample_content.clone(),
            host: draft.host.clone(),
            path: draft.path.clone(),
            sampling_mode: draft.sampling_mode,
            line_count: draft.line_count,
            file_size_bytes: draft.file_size_bytes,
            processing_status: draft.processing_status.clone(),
            error_details: draft.error_details.clone(),
        };

        match repository.store_sample(&sample.target_id, &sample).await? {
            StoreOutcome::Inserted => {
                info!(
                    target_id = %sample.target_id,
                    source_file = %sample.source_file,
                    status = sample.processing_status.as_str(),
                    "stored sampled log record"
                );

                if preprocessing_config.enabled {
                    run_preprocessing(
                        &repository,
                        &preprocessing_config,
                        &sample,
                    )
                    .await;
                }
            }
            StoreOutcome::Duplicate => {
                info!(
                    target_id = %sample.target_id,
                    source_file = %sample.source_file,
                    "skipping duplicate sample"
                );
            }
        }
    }

    Ok(())
}

/// Run the synchronous preprocessing pipeline inside `spawn_blocking` and
/// persist the resulting [`SampleMetadata`] to MongoDB.
///
/// Preprocessing failures are logged but never propagate — a metadata write
/// failure must never abort the sampling cycle for other files or targets.
async fn run_preprocessing(
    repository: &MongoRepository,
    config: &PreprocessingConfig,
    sample: &SampleRecord,
) {
    let preprocessor = Preprocessor::new(config.clone());
    let content = sample.sample_content.clone();
    let sample_hash = sample.sample_hash.clone();
    let target_id = sample.target_id.clone();

    // The pipeline is CPU-bound (regex + JSON scanning); push it off the
    // async executor so we do not block other tasks in the same thread pool.
    let started = Instant::now();

    let metadata = match tokio::task::spawn_blocking(move || {
        preprocessor.run(&sample_hash, &target_id, &content)
    })
    .await
    {
        Ok(metadata) => metadata,
        Err(error) => {
            error!(error = ?error, "preprocessing task panicked");
            metrics::record_error();
            return;
        }
    };

    let elapsed_secs = started.elapsed().as_secs_f64();
    metrics::record_duration(elapsed_secs);

    let worth = metadata.ingestion_hints.worth_classifying;
    let score = metadata.agentic_scan.signal_score;
    let stored_hash = metadata.sample_hash.clone();
    let stored_target = metadata.target_id.clone();

    match repository.store_metadata(&metadata).await {
        Ok(()) => {
            // Record one processed event per successful pipeline + write cycle.
            // `worth=true` also bumps the agentic_signals counter.
            metrics::record_processed(worth);

            info!(
                sample_hash = %stored_hash,
                target_id = %stored_target,
                worth_classifying = worth,
                signal_score = score,
                elapsed_ms = (elapsed_secs * 1000.0) as u64,
                "stored preprocessing metadata"
            );
        }
        Err(error) => {
            error!(
                error = %error,
                sample_hash = %stored_hash,
                "failed to store preprocessing metadata"
            );
            metrics::record_error();
        }
    }
}
