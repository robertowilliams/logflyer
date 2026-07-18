use std::sync::atomic::Ordering;

use axum::{extract::{Path, Query, State}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::SharedState;
use crate::config::AdminSettings;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SettingsResponse {
    /// Current effective configuration. Sensitive fields are masked with "***"
    /// when they are non-empty; an empty string means the value is not set.
    pub settings: AdminSettings,
    /// Whether any overrides are persisted in MongoDB (i.e. the admin UI has
    /// been used to save at least once).
    pub has_overrides: bool,
    /// True when this process started with an unconfirmed pending config and
    /// the 15-second rollback timer is still running. The frontend should
    /// immediately call POST /api/v1/admin/confirm when it sees this flag.
    pub pending_confirmation: bool,
}

#[derive(Serialize)]
pub struct SaveResponse {
    pub saved:            bool,
    pub restart_required: bool,
}

#[derive(Serialize)]
pub struct RestartResponse {
    pub accepted: bool,
    pub message:  &'static str,
}

// ── GET /api/v1/admin/settings ────────────────────────────────────────────────

pub async fn get_settings(
    State(state): State<SharedState>,
) -> Result<Json<SettingsResponse>, StatusCode> {
    let cfg = &state.config;

    let effective = AdminSettings {
        // ── MongoDB ───────────────────────────────────────────────────────────
        // Mask URI: may contain credentials, show *** when non-empty
        mongodb_uri:               Some(if cfg.mongo.uri.is_empty() { String::new() } else { "***".to_string() }),
        source_db_name:            Some(cfg.mongo.source_db_name.clone()),
        source_collection_name:    Some(cfg.mongo.source_collection_name.clone()),
        destination_db_name:       Some(cfg.mongo.destination_db_name.clone()),
        tracking_db_name:          Some(cfg.mongo.tracking_db_name.clone()),
        tracking_collection_name:  Some(cfg.mongo.tracking_collection_name.clone()),
        // ── Sampling ──────────────────────────────────────────────────────────
        sample_mode:               Some(cfg.sampling.mode.as_str().to_string()),
        sample_line_count:         Some(cfg.sampling.line_count as u64),
        // ── Service ───────────────────────────────────────────────────────────
        run_mode:                  Some(cfg.service.run_mode.to_string()),
        poll_interval_secs:        Some(cfg.service.poll_interval_secs),
        concurrency:               Some(cfg.service.concurrency as u64),
        ssh_timeout_secs:          Some(cfg.service.ssh_timeout_secs),
        api_port:                  Some(cfg.service.api_port as u64),
        // ── Discovery ─────────────────────────────────────────────────────────
        remote_max_depth:          Some(cfg.discovery.max_depth as u64),
        remote_max_files_per_target: Some(cfg.discovery.max_files_per_target as u64),
        remote_find_patterns:      Some(cfg.discovery.find_patterns.join(",")),
        // ── Preprocessing ─────────────────────────────────────────────────────
        preprocessing_enabled:           Some(cfg.preprocessing.enabled),
        preprocessing_agentic_threshold: Some(cfg.preprocessing.agentic_threshold),
        preprocessing_max_schema_lines:  Some(cfg.preprocessing.max_schema_lines as u64),
        metrics_port:                    Some(cfg.preprocessing.metrics_port as u64),
        entity_extraction_enabled:       Some(cfg.preprocessing.entity_extraction_enabled),
        entity_extraction_min_entities:  Some(cfg.preprocessing.min_entities_for_persist as u64),
        // ── Output adapters ───────────────────────────────────────────────────
        graph_writer_enabled:            Some(cfg.output.graph_writer_enabled),
        graph_writer_backend:            Some(cfg.output.graph_writer_backend.clone()),
        vector_writer_enabled:           Some(cfg.output.vector_writer_enabled),
        vector_writer_backend:           Some(cfg.output.vector_writer_backend.clone()),
        // ── Classification ────────────────────────────────────────────────────
        classification_enabled:           Some(cfg.classification.enabled),
        anthropic_api_key:                Some(
            if cfg.classification.api_key.is_empty() { String::new() } else { "***".to_string() }
        ),
        classification_model:             Some(cfg.classification.model.clone()),
        classification_signal_threshold:  Some(cfg.classification.signal_threshold),
        classification_max_per_cycle:     Some(cfg.classification.max_per_cycle as u64),
        classification_max_output_tokens: Some(cfg.classification.max_output_tokens as u64),
        classification_api_base_url:      Some(cfg.classification.api_base_url.clone()),
        classification_api_format:        Some(cfg.classification.api_format.clone()),
        // ── Notifications ─────────────────────────────────────────────────────
        notification_enabled:             Some(cfg.notification.enabled),
        notification_severity_threshold:  Some(cfg.notification.severity_threshold.as_str().to_string()),
        slack_webhook_url:                Some(cfg.notification.slack_webhook_url.clone().unwrap_or_default()),
        webhook_url:                      Some(cfg.notification.webhook_url.clone().unwrap_or_default()),
        webhook_secret:                   Some(
            if cfg.notification.webhook_secret.is_some() { "***".to_string() } else { String::new() }
        ),
        // ── Logging ───────────────────────────────────────────────────────────
        log_level:               Some(cfg.logging.level.clone()),
        log_directory:           Some(cfg.logging.directory.display().to_string()),
        log_file_base_name:      Some(cfg.logging.file_base_name.clone()),
        log_max_file_size_bytes: Some(cfg.logging.max_file_size_bytes as u64),
        log_max_files:           Some(cfg.logging.max_files as u64),
        // ── Config history ────────────────────────────────────────────────────
        config_history_enabled:         Some(cfg.config_history.enabled),
        config_history_master_key:      Some(
            if cfg.config_history.master_key.is_some() { "***".to_string() } else { String::new() }
        ),
        config_history_key_id:          Some(cfg.config_history.key_id.clone()),
        config_history_collection_name: Some(cfg.config_history.collection_name.clone()),
    };

    let has_overrides = state
        .repo
        .load_admin_settings()
        .await
        .ok()
        .flatten()
        .is_some();

    let pending_confirmation = state.pending_confirmation.load(Ordering::Relaxed);

    Ok(Json(SettingsResponse { settings: effective, has_overrides, pending_confirmation }))
}

// ── PUT /api/v1/admin/settings ────────────────────────────────────────────────

pub async fn put_settings(
    State(state): State<SharedState>,
    Json(mut incoming): Json<AdminSettings>,
) -> Result<Json<SaveResponse>, StatusCode> {
    // Load existing stored overrides so we can:
    //   a) preserve masked fields the user left as "***"
    //   b) capture the last *confirmed* config as the rollback target
    let existing = state
        .repo
        .load_admin_settings()
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    // Preserve masked sensitive fields when the UI sends "***" back unchanged.
    if incoming.anthropic_api_key.as_deref() == Some("***") {
        incoming.anthropic_api_key = existing.anthropic_api_key.clone();
    }
    if incoming.webhook_secret.as_deref() == Some("***") {
        incoming.webhook_secret = existing.webhook_secret.clone();
    }
    if incoming.config_history_master_key.as_deref() == Some("***") {
        incoming.config_history_master_key = existing.config_history_master_key.clone();
    }
    if incoming.mongodb_uri.as_deref() == Some("***") {
        incoming.mongodb_uri = existing.mongodb_uri.clone();
    }

    // Determine the rollback target: always the last confirmed settings.
    // If the current stored config is itself unconfirmed, dig out its _previous.
    let rollback_target = match state.repo.load_config_pending_meta().await.unwrap_or(None) {
        Some((true, _))           => Some(existing.clone()),  // current is confirmed
        Some((false, Some(prev))) => Some(prev),              // use the stored previous
        _                         => None,                    // nothing saved yet
    };

    if state.config.config_history.enabled {
        if let Err(e) = state
            .repo
            .save_admin_settings_history(
                &incoming,
                &state.config.config_history,
                "admin settings save",
            )
            .await
        {
            error!(error = %e, "failed to save admin settings history");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Save with _confirmed = false and rollback target embedded.
    if let Err(e) = state
        .repo
        .save_admin_settings_pending(&incoming, rollback_target)
        .await
    {
        error!(error = %e, "failed to save admin settings");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    info!("admin settings saved (pending confirmation)");
    Ok(Json(SaveResponse { saved: true, restart_required: true }))
}

// ── POST /api/v1/admin/restart ────────────────────────────────────────────────
//
// Triggers a clean process exit after flushing the response.  Docker's
// `restart: unless-stopped` (or `always`) policy will immediately bring the
// container back up, re-reading the latest config from MongoDB on startup.

pub async fn restart() -> (StatusCode, Json<RestartResponse>) {
    info!("admin restart requested — process will exit in 500 ms");

    // Spawn a task that exits the process slightly after this response is sent.
    // 500 ms is enough for Axum to flush the response body to the client.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        info!("restarting now (process exit 0)");
        std::process::exit(0);
    });

    (
        StatusCode::ACCEPTED,
        Json(RestartResponse {
            accepted: true,
            message:  "restart accepted — container will be back in a few seconds",
        }),
    )
}

// ── POST /api/v1/admin/confirm ────────────────────────────────────────────────
//
// Called by the frontend as soon as it can successfully reach the service after
// a restart.  Marks the current config as confirmed and disarms the rollback timer.

pub async fn confirm_settings(
    State(state): State<SharedState>,
) -> StatusCode {
    if let Err(e) = state.repo.confirm_admin_settings().await {
        error!(error = %e, "failed to confirm admin settings");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // Clear the in-process flag so subsequent GET /settings shows confirmed.
    state.pending_confirmation.store(false, Ordering::Relaxed);
    info!("admin settings confirmed by frontend — rollback timer disarmed");
    StatusCode::OK
}

// ── GET /api/v1/admin/settings/history ───────────────────────────────────────

#[derive(Serialize)]
pub struct HistoryEntry {
    pub version:    i64,
    pub created_at: String,
    pub created_by: String,
    pub source:     String,
    pub reason:     String,
    pub key_id:     String,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub entries: Vec<HistoryEntry>,
}

pub async fn get_settings_history(
    State(state): State<SharedState>,
) -> Result<Json<HistoryResponse>, StatusCode> {
    if !state.config.config_history.enabled {
        return Ok(Json(HistoryResponse { entries: vec![] }));
    }

    let docs = state
        .repo
        .list_config_history(&state.config.config_history.collection_name, 50)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to list config history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let entries = docs
        .into_iter()
        .filter_map(|d| {
            let version    = d.get_i64("version").ok()?;
            let created_at = d.get_datetime("created_at")
                .map(|dt| dt.to_system_time())
                .ok()
                .and_then(|st| {
                    let secs = st.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                    // ISO-8601 UTC string
                    Some(format_unix_ts(secs))
                })
                .unwrap_or_default();
            let created_by = d.get_str("created_by").unwrap_or("").to_string();
            let source     = d.get_str("source").unwrap_or("").to_string();
            let reason     = d.get_str("reason").unwrap_or("").to_string();
            let key_id     = d.get_document("encryption")
                .and_then(|enc| enc.get_str("key_id"))
                .unwrap_or("")
                .to_string();
            Some(HistoryEntry { version, created_at, created_by, source, reason, key_id })
        })
        .collect();

    Ok(Json(HistoryResponse { entries }))
}

fn format_unix_ts(secs: u64) -> String {
    // Minimal hand-rolled formatter to avoid pulling in chrono just for this.
    // Input: seconds since Unix epoch. Output: "YYYY-MM-DDTHH:MM:SSZ".
    let s = secs;
    let sec  = s % 60;
    let min  = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;

    // Compute year/month/day from days since 1970-01-01 (Gregorian proleptic)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm: http://howardhinnant.github.io/date_algorithms.html (civil_from_days)
    let z  = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y   = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp  = (5 * doy + 2) / 153;
    let d   = doy - (153 * mp + 2) / 5 + 1;
    let m   = if mp < 10 { mp + 3 } else { mp - 9 };
    let y   = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ── POST /api/v1/admin/settings/restore/:version ──────────────────────────────

#[derive(Serialize)]
pub struct RestoreResponse {
    pub restored: bool,
    pub version:  i64,
}

pub async fn post_settings_restore(
    State(state): State<SharedState>,
    Path(version): Path<i64>,
) -> Result<Json<RestoreResponse>, StatusCode> {
    let history_cfg = &state.config.config_history;

    if !history_cfg.enabled {
        warn!("restore requested but config history is disabled");
        return Err(StatusCode::NOT_FOUND);
    }

    let master_key = match history_cfg.master_key.as_deref() {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => {
            error!("restore requested but CONFIG_HISTORY_MASTER_KEY is not set");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let restored_settings = state
        .repo
        .restore_config_history_entry(
            &history_cfg.collection_name,
            version,
            &master_key,
        )
        .await
        .map_err(|e| {
            error!(error = %e, version, "failed to restore config history entry");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Save the restored settings as the new active overrides.
    if let Err(e) = state.repo.save_admin_settings(&restored_settings).await {
        error!(error = %e, version, "failed to save restored admin settings");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Also snapshot the restore action itself into history.
    if let Err(e) = state
        .repo
        .save_admin_settings_history(
            &restored_settings,
            history_cfg,
            &format!("restored from version {version}"),
        )
        .await
    {
        // Non-fatal: the restore itself succeeded.
        warn!(error = %e, version, "failed to record restore in config history");
    }

    info!(version, "admin settings restored from config history");
    Ok(Json(RestoreResponse { restored: true, version }))
}

// ── GET /api/v1/admin/models ──────────────────────────────────────────────────
//
// Proxy: fetches GET {base_url}/v1/models server-side (avoids browser CORS).
// Query params:
//   base_url — optional; falls back to config.classification.api_base_url
//   api_key  — optional; "***" means "use the stored key from config"

#[derive(Deserialize)]
pub struct ModelsQuery {
    pub base_url: Option<String>,
    pub api_key:  Option<String>,
}

#[derive(Serialize)]
pub struct ModelsResponse {
    pub ok:     bool,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error:  Option<String>,
}

pub async fn get_models(
    State(state): State<SharedState>,
    Query(params): Query<ModelsQuery>,
) -> Json<ModelsResponse> {
    // Resolve base URL: param → config → provider default
    let base_url = params
        .base_url
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| state.config.classification.api_base_url.clone());

    let base_url = if base_url.is_empty() {
        // Pick a sensible default based on format
        if state.config.classification.api_format == "anthropic" {
            "https://api.anthropic.com".to_string()
        } else {
            "https://api.openai.com".to_string()
        }
    } else {
        base_url.trim_end_matches('/').to_string()
    };

    // Resolve API key: "***" or missing → use the key stored in running config
    let api_key = match params.api_key.as_deref() {
        Some("***") | Some("") | None => state.config.classification.api_key.clone(),
        Some(k) => k.to_string(),
    };

    if api_key.is_empty() {
        return Json(ModelsResponse {
            ok:     false,
            models: vec![],
            error:  Some("no API key configured".to_string()),
        });
    }

    let url = format!("{base_url}/v1/models");

    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build HTTP client for model fetch");
            return Json(ModelsResponse {
                ok:     false,
                models: vec![],
                error:  Some(format!("client build error: {e}")),
            });
        }
    };

    let resp = match http
        .get(&url)
        .header("Authorization",  format!("Bearer {api_key}"))
        .header("content-type",   "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(url = %url, error = %e, "model list request failed");
            return Json(ModelsResponse {
                ok:     false,
                models: vec![],
                error:  Some(format!("request failed: {e}")),
            });
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!(url = %url, %status, "model list endpoint returned error");
        return Json(ModelsResponse {
            ok:     false,
            models: vec![],
            error:  Some(format!("HTTP {status}: {body}")),
        });
    }

    // Parse as a generic JSON value — be tolerant of unexpected shapes
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(url = %url, error = %e, "model list response was not valid JSON");
            return Json(ModelsResponse {
                ok:     false,
                models: vec![],
                error:  Some(format!("unexpected response format: {e}")),
            });
        }
    };

    // Extract IDs from the OpenAI-compatible `data` array
    let models: Vec<String> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if models.is_empty() {
        warn!(url = %url, "model list returned zero models");
        return Json(ModelsResponse {
            ok:     false,
            models: vec![],
            error:  Some("provider returned an empty model list".to_string()),
        });
    }

    Json(ModelsResponse { ok: true, models, error: None })
}
