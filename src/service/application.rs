use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use mongodb::bson::DateTime;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::classification::ClassificationWorker;
use crate::config::{AppConfig, EmbeddingConfig, OutputConfig, PreprocessingConfig, RunMode};
use crate::embedding::EmbeddingWorker;
use crate::error::AppError;
use crate::metrics;
use crate::models::{RawTargetDocument, SampleRecord, ValidatedTarget};
use crate::notification::NotificationWorker;
use crate::output::{graph::GraphWriter, vector::VectorWriter};
use crate::preprocessing::Preprocessor;
use crate::repository::{MongoRepository, StoreOutcome};
use crate::ssh::SshLogInspector;
use crate::utils::compute_sample_hash;

/// Bundle of optional async output adapters built once per `Application`.
///
/// Each adapter is independently gated by its own config flag, so any
/// combination of `(graph, vector, embedding)` may be present.  Cloning the
/// struct is cheap — only the inner `Arc`s are bumped.
#[derive(Clone, Default)]
struct AsyncOutputs {
    graph_writer:     Option<Arc<GraphWriter>>,
    vector_writer:    Option<Arc<VectorWriter>>,
    embedding_worker: Option<Arc<EmbeddingWorker>>,
    /// `embedding.model` captured at construction so the spawned task
    /// doesn't have to clone the full `EmbeddingConfig`.
    embedding_model:  String,
    /// `output.min_entities_for_persist` snapshot used to skip writes for
    /// samples below the threshold (saves DB round-trips on sparse samples).
    min_entities_for_persist: usize,
}

#[derive(Clone)]
pub struct Application {
    config: AppConfig,
    repository: MongoRepository,
    inspector: Arc<SshLogInspector>,
    classification_worker: Option<Arc<ClassificationWorker>>,
    async_outputs: AsyncOutputs,
    /// Shared with the API: a POST /api/v1/sample notifies this to run a cycle immediately.
    pub trigger: Arc<Notify>,
}

/// Result of a single `Application::smoketest_sample` invocation.
///
/// Reports both *what the pipeline produced* (entity_count, relation_count
/// taken from the persisted `SampleMetadata`) and *what landed in each
/// auxiliary collection*, scoped to the sample_hash so the numbers reflect
/// the current run rather than historical data.
#[derive(Debug, Clone)]
pub struct SmokeTestReport {
    pub sample_hash:                 String,
    /// `true` if the sample was newly inserted; `false` on a duplicate.
    pub sample_was_new:              bool,
    /// Entity / relation counts pulled from the freshly stored
    /// `sample_metadata` document.
    pub entity_count:                u64,
    pub relation_count:              u64,
    /// Documents now in `entity_edges` for this sample_hash.
    pub edges_collection_total:      u64,
    /// Documents now in `prov_relations` for this sample_hash.
    pub prov_collection_total:       u64,
    /// Documents now in `otel_spans` for this sample_hash.
    pub spans_collection_total:      u64,
    /// Documents now in `content_embeddings` + `behavioral_embeddings`
    /// for this sample_hash.
    pub embeddings_collection_total: u64,
    /// Snapshot of the kill-switches at run time so the report explains
    /// itself ("graph writer disabled — that's why edges_total=0").
    pub graph_writer_enabled:        bool,
    pub vector_writer_enabled:       bool,
    pub embedding_enabled:           bool,
    pub entity_extraction_enabled:   bool,
    pub min_entities_for_persist:    usize,
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

        let async_outputs = build_async_outputs(
            &config.output,
            &config.embedding,
            &config.preprocessing,
            &repository,
        );

        Ok(Self {
            config,
            repository,
            inspector,
            classification_worker,
            async_outputs,
            trigger: Arc::new(Notify::new()),
        })
    }

    /// Run the full preprocessing + async-output pipeline against a single
    /// in-memory sample, bypassing SSH and the target loop.  Used by the
    /// `logflayer smoketest <fixture>` subcommand to verify end-to-end wiring
    /// without needing an SSH-reachable target.
    ///
    /// Inserts the synthetic sample into the destination collection first
    /// (so the API/UI can find it via the existing `samples` endpoints), then
    /// runs `run_preprocessing` exactly as the live cycle would, then queries
    /// the auxiliary collections (filtered by sample_hash) so the report
    /// reflects only this run.
    pub async fn smoketest_sample(
        &self,
        sample_content: String,
        target_id: String,
        source_label: String,
    ) -> Result<SmokeTestReport, AppError> {
        use crate::models::{ProcessingStatus, SampleDraft};
        use crate::sampling::SamplingMode;

        // Build a draft so we can reuse the canonical hash function.  Hash is
        // deterministic over (target_id, source_file, mode, content, status,
        // error) so re-running the smoketest with identical inputs is a no-op
        // upsert at the metadata layer.
        let draft = SampleDraft {
            target_id: target_id.clone(),
            source_file: source_label.clone(),
            sample_content: sample_content.clone(),
            host: "localhost".to_string(),
            path: source_label.clone(),
            sampling_mode: SamplingMode::Both,
            line_count: Some(sample_content.lines().count() as u64),
            file_size_bytes: Some(sample_content.len() as u64),
            processing_status: ProcessingStatus::Stored,
            error_details: None,
        };
        let sample_hash = crate::utils::compute_sample_hash(&draft);

        let sample = SampleRecord {
            timestamp: DateTime::now(),
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
            sample_hash: sample_hash.clone(),
        };

        // Insert sample (duplicate is fine — re-running the smoketest twice
        // should not fail).
        let inserted = matches!(
            self.repository.store_sample(&sample.target_id, &sample).await?,
            StoreOutcome::Inserted
        );

        // Run the live preprocessing path.
        run_preprocessing(
            &self.repository,
            &self.config.preprocessing,
            &self.classification_worker,
            &self.async_outputs,
            &sample,
        )
        .await;

        // Count what landed in each auxiliary collection for this sample.
        let (_edges_records, edges_total) = self
            .repository
            .fetch_edges_page(Some(&sample_hash), None, 1, 0)
            .await
            .unwrap_or_default();
        let (_prov_records, prov_total) = self
            .repository
            .fetch_prov_page(Some(&sample_hash), None, None, 1, 0)
            .await
            .unwrap_or_default();
        let (_spans_records, spans_total) = self
            .repository
            .fetch_spans_page(Some(&sample_hash), None, 1, 0)
            .await
            .unwrap_or_default();

        let metadata_doc = self
            .repository
            .fetch_metadata_by_hash(&sample_hash)
            .await
            .ok()
            .flatten();

        // Count embedding records by querying the destination DB directly.
        // We don't expose a dedicated repository method since this is the
        // only caller; if more callers materialise we should promote it.
        let embeddings_total: u64 = {
            use mongodb::bson::{doc, Document};
            let db = self.repository.destination_db();
            let filter = doc! { "sample_hash": &sample_hash };
            let content = db
                .collection::<Document>("content_embeddings")
                .count_documents(filter.clone(), None)
                .await
                .unwrap_or(0);
            let behavioral = db
                .collection::<Document>("behavioral_embeddings")
                .count_documents(filter, None)
                .await
                .unwrap_or(0);
            content + behavioral
        };

        let (entity_count, relation_count) = match &metadata_doc {
            Some(doc) => (
                doc.get("entity_count").and_then(|v| v.as_u64()).unwrap_or(0),
                doc.get("relation_count").and_then(|v| v.as_u64()).unwrap_or(0),
            ),
            None => (0, 0),
        };

        Ok(SmokeTestReport {
            sample_hash,
            sample_was_new: inserted,
            entity_count,
            relation_count,
            edges_collection_total: edges_total,
            prov_collection_total: prov_total,
            spans_collection_total: spans_total,
            embeddings_collection_total: embeddings_total,
            graph_writer_enabled: self.config.output.graph_writer_enabled,
            vector_writer_enabled: self.config.output.vector_writer_enabled,
            embedding_enabled: self.config.embedding.enabled,
            entity_extraction_enabled: self.config.preprocessing.entity_extraction_enabled,
            min_entities_for_persist: self.config.preprocessing.min_entities_for_persist,
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
        let async_outputs = self.async_outputs.clone();

        stream::iter(documents)
            .for_each_concurrent(self.config.service.concurrency, move |document| {
                let repository = repository.clone();
                let inspector = Arc::clone(&inspector);
                let preprocessing_config = preprocessing_config.clone();
                let classification_worker = classification_worker.clone();
                let async_outputs = async_outputs.clone();

                async move {
                    if let Err(error) = process_document(
                        repository,
                        inspector,
                        preprocessing_config,
                        classification_worker,
                        async_outputs,
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
    async_outputs: AsyncOutputs,
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
                        &async_outputs,
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
    async_outputs: &AsyncOutputs,
    sample: &SampleRecord,
) {
    let preprocessor = Preprocessor::new(config.clone());
    let content = sample.sample_content.clone();
    let sample_hash = sample.sample_hash.clone();
    let target_id = sample.target_id.clone();

    let started = Instant::now();

    let pipeline_output = match tokio::task::spawn_blocking(move || {
        preprocessor.run(&sample_hash, &target_id, &content)
    })
    .await
    {
        Ok(out) => out,
        Err(error) => {
            error!(error = ?error, "preprocessing task panicked");
            metrics::record_error();
            return;
        }
    };

    let elapsed_secs = started.elapsed().as_secs_f64();
    metrics::record_duration(elapsed_secs);

    let metadata = pipeline_output.metadata;
    let prov_triples = pipeline_output.prov_triples;
    let otel_spans = pipeline_output.otel_spans;
    let task_correlation = pipeline_output.task_correlation;

    let worth = metadata.ingestion_hints.worth_classifying;
    let score = metadata.agentic_scan.signal_score;
    let stored_hash = metadata.sample_hash.clone();
    let stored_target = metadata.target_id.clone();

    match repository.store_metadata(&metadata).await {
        Ok(()) => {
            metrics::record_processed(worth);
            metrics::record_format_detected(metadata.format.log_type.as_str());
            if metadata.schema.is_some() {
                metrics::record_schema_extracted();
            }
            info!(
                sample_hash = %stored_hash,
                target_id = %stored_target,
                worth_classifying = worth,
                signal_score = score,
                elapsed_ms = (elapsed_secs * 1000.0) as u64,
                "stored preprocessing metadata"
            );

            // Stage 11: fold this sample into its task. Independent of the
            // min_entities gate below — a task should register even when the
            // sample was too sparse to warrant graph/vector writes, or the task's
            // sample list would have holes in it.
            if let Some(ref correlation) = task_correlation {
                upsert_task_for_sample(repository, &metadata, correlation).await;
            }

            // Skip the async output adapters when the sample produced fewer
            // entities than the configured threshold — saves DB round-trips
            // on sparse / borderline-agentic samples.
            if metadata.entities.len() >= async_outputs.min_entities_for_persist {
                // Persist the auxiliary lineage data via the optional GraphWriter.
                // Failures here are logged but do NOT propagate — they must not
                // block the sampling loop or downstream classification.
                persist_lineage(
                    async_outputs,
                    &stored_hash,
                    &metadata.relations,
                    &prov_triples,
                    &otel_spans,
                )
                .await;

                // Generate + persist embeddings (independent of the graph writer).
                persist_embeddings(
                    async_outputs,
                    &stored_hash,
                    &sample.sample_content,
                    &metadata.entities,
                    &metadata.relations,
                    metadata.agentic_scan.signal_score,
                )
                .await;
            }

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

// ─── Async output wiring helpers ──────────────────────────────────────────────

/// Construct optional [`GraphWriter`], [`VectorWriter`], and [`EmbeddingWorker`]
/// based on `output` + `embedding` config flags.
///
/// Each adapter is built independently — a failure in one logs a warning and
/// disables that adapter without preventing the others from coming online.
fn build_async_outputs(
    output: &OutputConfig,
    embedding: &EmbeddingConfig,
    preprocessing: &PreprocessingConfig,
    repository: &MongoRepository,
) -> AsyncOutputs {
    let graph_writer = if output.graph_writer_enabled {
        if output.graph_writer_backend != "mongodb" {
            warn!(
                backend = %output.graph_writer_backend,
                "GRAPH_WRITER_BACKEND is not 'mongodb' — falling back to MongoDB writer; \
                 alternative backends are not yet implemented"
            );
        }
        info!("graph writer enabled (backend=mongodb)");
        Some(Arc::new(GraphWriter::new(repository.destination_db())))
    } else {
        info!("graph writer disabled (GRAPH_WRITER_ENABLED=false)");
        None
    };

    let vector_writer = if output.vector_writer_enabled {
        if output.vector_writer_backend != "mongodb" {
            warn!(
                backend = %output.vector_writer_backend,
                "VECTOR_WRITER_BACKEND is not 'mongodb' — falling back to MongoDB writer; \
                 alternative backends are not yet implemented"
            );
        }
        info!("vector writer enabled (backend=mongodb)");
        Some(Arc::new(VectorWriter::new(repository.destination_db())))
    } else {
        info!("vector writer disabled (VECTOR_WRITER_ENABLED=false)");
        None
    };

    // The embedding worker only makes sense if at least one persistence
    // path will consume its output (vector_writer).  Build it unconditionally
    // — content embedding still no-ops when `embedding.enabled = false`,
    // and behavioral embedding is local + cheap.  But skip building if the
    // `EmbeddingConfig` itself is fully disabled AND no vector writer is
    // wired (no-one would consume the output).
    let embedding_worker = if vector_writer.is_some() || embedding.enabled {
        match EmbeddingWorker::new(embedding.clone()) {
            Ok(w) => {
                info!(
                    "embedding worker built (content_enabled={}, model={})",
                    embedding.enabled, embedding.model,
                );
                Some(Arc::new(w))
            }
            Err(e) => {
                error!(error = %e, "failed to build EmbeddingWorker — embeddings disabled");
                None
            }
        }
    } else {
        info!("embedding worker not built (no vector writer + EMBEDDING_ENABLED=false)");
        None
    };

    AsyncOutputs {
        graph_writer,
        vector_writer,
        embedding_worker,
        embedding_model: embedding.model.clone(),
        min_entities_for_persist: preprocessing.min_entities_for_persist,
    }
}

/// Best-effort persistence of relation edges, PROV triples, and OTel spans
/// via the optional [`GraphWriter`].
///
/// Each write is independent: a failure on one is logged and processing
/// Fold one sample into its task's `tasks` document (Stage 11).
///
/// Checks whether the sample has already been counted before upserting, because
/// `$addToSet` is idempotent but `$inc` is not — re-processing a sample would
/// otherwise keep inflating the task's entity and relation totals.
///
/// Errors are logged and swallowed. A task index that is briefly behind is far
/// preferable to a failed sampling cycle, and the next run of the same sample
/// repairs it.
async fn upsert_task_for_sample(
    repository: &MongoRepository,
    metadata: &crate::models::SampleMetadata,
    correlation: &crate::preprocessing::task_correlator::TaskCorrelation,
) {
    let already_counted = match repository
        .task_sample_counted(&correlation.task_id, &metadata.sample_hash)
        .await
    {
        Ok(counted) => counted,
        Err(error) => {
            warn!(error = ?error, task_id = %correlation.task_id, "could not check task membership; skipping task upsert");
            return;
        }
    };

    // Re-run of a sample we have already folded in: refresh the sets and
    // last_seen, but do not advance the counters again.
    let (entity_delta, relation_delta) = if already_counted {
        (0, 0)
    } else {
        (metadata.entity_count, metadata.relation_count)
    };

    if let Err(error) = repository
        .upsert_task(
            &correlation.task_id,
            &correlation.source,
            correlation.correlation_key.as_deref(),
            &metadata.sample_hash,
            &metadata.otel_trace_id,
            &metadata.target_id,
            entity_delta,
            relation_delta,
        )
        .await
    {
        warn!(error = ?error, task_id = %correlation.task_id, "failed to upsert task");
        return;
    }

    info!(
        task_id = %correlation.task_id,
        task_id_source = %correlation.source,
        sample_hash = %metadata.sample_hash,
        new_to_task = !already_counted,
        "folded sample into task"
    );
}

/// continues with the next.  All errors are non-fatal.
async fn persist_lineage(
    outputs: &AsyncOutputs,
    sample_hash: &str,
    relations: &[crate::models::RelationEdge],
    prov_triples: &[crate::preprocessing::prov_linker::ProvTriple],
    otel_spans: &[crate::preprocessing::otel_builder::OtelSpan],
) {
    let Some(writer) = outputs.graph_writer.as_ref() else {
        return;
    };

    // Skip empty samples to save round-trips on logs that produced no entities.
    if relations.is_empty() && prov_triples.is_empty() && otel_spans.is_empty() {
        return;
    }

    if !relations.is_empty() {
        match writer.write_edges(relations).await {
            Ok(n) => info!(sample_hash = %sample_hash, count = n, "wrote relation edges"),
            Err(e) => warn!(sample_hash = %sample_hash, error = %e, "graph writer: write_edges failed"),
        }
    }
    if !prov_triples.is_empty() {
        match writer.write_prov(prov_triples).await {
            Ok(n) => info!(sample_hash = %sample_hash, count = n, "wrote prov triples"),
            Err(e) => warn!(sample_hash = %sample_hash, error = %e, "graph writer: write_prov failed"),
        }
    }
    if !otel_spans.is_empty() {
        match writer.write_spans(otel_spans).await {
            Ok(n) => info!(sample_hash = %sample_hash, count = n, "wrote otel spans"),
            Err(e) => warn!(sample_hash = %sample_hash, error = %e, "graph writer: write_spans failed"),
        }
    }
}

/// Best-effort embedding generation + vector-store persistence.
///
/// No-ops when the vector writer is not wired or the entity count is below
/// `min_entities_for_persist`.  Errors are logged and never propagated.
async fn persist_embeddings(
    outputs: &AsyncOutputs,
    sample_hash: &str,
    content: &str,
    entities: &[crate::models::EntityRecord],
    relations: &[crate::models::RelationEdge],
    signal_score: f64,
) {
    // No vector writer ⇒ nothing to do (content embeddings would have nowhere
    // to land, behavioral too).
    let Some(vector_writer) = outputs.vector_writer.as_ref() else {
        return;
    };
    let Some(worker) = outputs.embedding_worker.as_ref() else {
        return;
    };

    // The min-entities threshold is enforced by the caller (`run_preprocessing`)
    // — we only reach here when the gate is satisfied.

    let result = worker
        .embed_sample(sample_hash, content, entities, relations, signal_score)
        .await;

    let records = result.into_records(&outputs.embedding_model);
    match vector_writer.write(&records).await {
        Ok(n) => info!(sample_hash = %sample_hash, count = n, "wrote embedding records"),
        Err(e) => warn!(sample_hash = %sample_hash, error = %e, "vector writer: write failed"),
    }
}
