mod client;
mod prompt;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mongodb::bson::DateTime;
use tracing::{error, info, warn};

use crate::config::ClassificationConfig;
use crate::error::AppError;
use crate::models::{
    ClassificationRecord, ClassificationStatus, Finding, SampleMetadata, SampleRecord, Severity,
};
use crate::notification::NotificationWorker;
use crate::repository::MongoRepository;

use client::LlmClient;

pub const CLASSIFICATION_VERSION: &str = "1";

// ── Worker ────────────────────────────────────────────────────────────────────

pub struct ClassificationWorker {
    client:               LlmClient,
    repository:           MongoRepository,
    config:               ClassificationConfig,
    notification_worker:  Option<Arc<NotificationWorker>>,
    /// Counts API calls made in the current cycle; reset via `reset_cycle_counter`.
    cycle_counter:        Arc<AtomicUsize>,
}

impl ClassificationWorker {
    pub fn new(
        config:              ClassificationConfig,
        repository:          MongoRepository,
        notification_worker: Option<Arc<NotificationWorker>>,
    ) -> Result<Self, AppError> {
        let client = LlmClient::new(
            config.api_key.clone(),
            config.model.clone(),
            config.api_base_url.clone(),
            config.api_format.clone(),
        )?;
        Ok(Self {
            client,
            repository,
            config,
            notification_worker,
            cycle_counter: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Reset the per-cycle API call counter. Call this at the start of each
    /// sampling cycle so `max_per_cycle` is enforced per-cycle, not globally.
    pub fn reset_cycle_counter(&self) {
        self.cycle_counter.store(0, Ordering::Relaxed);
    }

    /// Attempt to classify a single sample.  Never panics or propagates errors
    /// to the caller — failures are logged and recorded in `sample_metadata`.
    pub async fn classify_sample(&self, sample: &SampleRecord, metadata: &SampleMetadata) {
        // Guard: master switch.
        if !self.config.enabled {
            return;
        }

        // Guard: per-cycle cap.
        let prev = self.cycle_counter.fetch_add(1, Ordering::Relaxed);
        if prev >= self.config.max_per_cycle {
            if prev == self.config.max_per_cycle {
                warn!("classification cap reached for this cycle (max_per_cycle={})",
                      self.config.max_per_cycle);
            }
            return;
        }

        // Guard: signal threshold.
        if metadata.agentic_scan.signal_score < self.config.signal_threshold {
            return;
        }

        // Guard: already classified.
        if metadata.classification_status == ClassificationStatus::Classified {
            return;
        }

        if let Err(e) = self.do_classify(sample, metadata).await {
            error!(
                target_id = %sample.target_id,
                sample_hash = %sample.sample_hash,
                error = %e,
                "classification failed"
            );
            let _ = self.repository
                .update_classification_status(&sample.sample_hash, ClassificationStatus::Failed)
                .await;
        }
    }

    async fn do_classify(
        &self,
        sample:   &SampleRecord,
        metadata: &SampleMetadata,
    ) -> Result<(), AppError> {
        let (system, user) = prompt::build(sample, metadata);

        let (text, input_tokens, output_tokens) = self.client
            .complete(&system, &user, self.config.max_output_tokens)
            .await?;

        let record = parse_response(
            &text,
            &sample.sample_hash,
            &sample.target_id,
            &self.config.model,
            input_tokens,
            output_tokens,
        )?;

        info!(
            target_id  = %record.target_id,
            sample_hash = %record.sample_hash,
            severity   = %record.severity.as_str(),
            confidence = record.confidence,
            input_tok  = input_tokens,
            output_tok = output_tokens,
            "classification stored"
        );

        self.repository.store_classification(&record).await?;
        self.repository
            .update_classification_status(&sample.sample_hash, ClassificationStatus::Classified)
            .await?;

        // Fire notifications (async, non-blocking — errors are logged inside).
        if let Some(notifier) = &self.notification_worker {
            notifier.notify(&record).await;
        }

        Ok(())
    }
}

// ── Response parsing ──────────────────────────────────────────────────────────

fn parse_response(
    text:         &str,
    sample_hash:  &str,
    target_id:    &str,
    model:        &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Result<ClassificationRecord, AppError> {
    // Strip optional markdown code fences the model may include.
    let json_text = strip_code_fence(text.trim());

    let v: serde_json::Value = serde_json::from_str(json_text).map_err(|e| {
        AppError::Classification(format!(
            "model returned non-JSON: {e}\nraw: {json_text}"
        ))
    })?;

    let severity = match v["severity"].as_str().unwrap_or("normal") {
        "critical" => Severity::Critical,
        "warning"  => Severity::Warning,
        "info"     => Severity::Info,
        _          => Severity::Normal,
    };

    let categories: Vec<String> = v["categories"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|c| c.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let summary = v["summary"].as_str().unwrap_or("No summary provided.").to_string();

    let key_findings: Vec<Finding> = v["key_findings"]
        .as_array()
        .map(|arr| {
            arr.iter().map(|f| Finding {
                pattern:  f["pattern"].as_str().unwrap_or("").to_string(),
                count:    f["count"].as_u64().unwrap_or(0) as u32,
                severity: f["severity"].as_str().unwrap_or("info").to_string(),
                example:  f["example"].as_str().unwrap_or("").to_string(),
            }).collect()
        })
        .unwrap_or_default();

    let recommendations: Vec<String> = v["recommendations"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|r| r.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let confidence = v["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);

    Ok(ClassificationRecord {
        sample_hash:            sample_hash.to_string(),
        target_id:              target_id.to_string(),
        classified_at:          DateTime::now(),
        model:                  model.to_string(),
        severity,
        categories,
        summary,
        key_findings,
        recommendations,
        confidence,
        input_tokens,
        output_tokens,
        classification_version: CLASSIFICATION_VERSION.to_string(),
    })
}

fn strip_code_fence(s: &str) -> &str {
    let s = s.trim_start_matches("```json").trim_start_matches("```");
    let s = s.trim_end_matches("```");
    s.trim()
}
