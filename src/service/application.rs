use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use mongodb::bson::DateTime;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::classification::ClassificationWorker;
use crate::config::{AppConfig, PreprocessingConfig, RunMode};
use crate::error::AppError;
use crate::metrics;
use crate::models::{RawTargetDocument, SampleRecord, ValidatedTarget};
use crate::notification::NotificationWorker;
use crate::preprocessing::Preprocessor;
use crate::repository::{MongoRepository, StoreOutcome};
use crate::ssh::SshLogInspector;
use crate::utils::compute_sample_hash;

#[derive(Clone)]
pub struct Application {
    config: AppConfig,
    repository: MongoRepository,
    inspector: Arc<SshLogInspector>,
    classification_worker: Option<Arc<ClassificationWorker>>,
    /// Shared with the API: a POST /api/v1/sample notifies this to run a cycle immediately.
    pub trigger: Arc<Notify>,
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

        let notification_worker: Option<Arc<NotificationWorker>> =
            if config.notification.enabled {
                info!(
                    severity_threshold = config.notification.severity_threshold.as_str(),
                    "notifications enabled"
                );
                Some(Arc::new(NotificationWorker::new(config.notification.clone())))
            } else {
                info!("notifications disabled (NOTIFICATION_ENABLED=false)");
                None
            };

        let classification_worker = if config.classification.enabled {
            match ClassificationWorker::new(
                config.classification.clone(),
                repository.clone(),
                notification_worker,
            ) {
                Ok(w) => {
                    info!("LLM classification enabled (model={})", config.classification.model);
                    Some(Arc::new(w))
                }
                Err(e) => {
                    error!(error = %e, "failed to build ClassificationWorker — classification disabled");
                    None
                }
            }
        } else {
            info!("LLM classification disabled (CLASSIFICATION_ENABLED=false)");
            None
        };

        Ok(Self {
            config,
            repository,
            inspector,
            classification_worker,
            trigger: Arc::new(Notify::new()),
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
                _ = self.trigger.notified() => {
                    info!("manual trigger received, running immediate sampling cycle");
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

        // Reset per-cycle classification cap at the start of every cycle.
        if let Some(w) = &self.classification_worker {
            w.reset_cycle_counter();
        }

        let repository = self.repository.clone();
        let inspector = Arc::clone(&self.inspector);
        let preprocessing_config = self.config.preprocessing.clone();
        let classification_worker = self.classification_worker.clone();

        stream::iter(documents)
            .for_each_concurrent(self.config.service.concurrency, move |document| {
                let repository = repository.clone();
                let inspector = Arc::clone(&inspector);
                let preprocessing_config = preprocessing_config.clone();
                let classification_worker = classification_worker.clone();

                async move {
                    if let Err(error) = process_document(
                        repository,
                        inspector,
                        preprocessing_config,
                        classification_worker,
                        document,
                    )
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
    classification_worker: Option<Arc<ClassificationWorker>>,
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
                        &classification_worker,
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

async fn run_preprocessing(
    repository: &MongoRepository,
    config: &PreprocessingConfig,
    classification_worker: &Option<Arc<ClassificationWorker>>,
    sample: &SampleRecord,
) {
    let preprocessor = Preprocessor::new(config.clone());
    let content = sample.sample_content.clone();
    let sample_hash = sample.sample_hash.clone();
    let target_id = sample.target_id.clone();

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
            metrics::record_processed(worth);
            info!(
                sample_hash = %stored_hash,
                target_id = %stored_target,
                worth_classifying = worth,
                signal_score = score,
                elapsed_ms = (elapsed_secs * 1000.0) as u64,
                "stored preprocessing metadata"
            );

            // Trigger classification for samples that exceed the signal threshold.
            // The worker itself checks enabled, worth_classifying, signal_score and
            // cycle cap — so we can call unconditionally.
            if let Some(worker) = classification_worker {
                worker.classify_sample(sample, &metadata).await;
            }
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
