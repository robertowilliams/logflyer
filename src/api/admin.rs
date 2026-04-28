use axum::{extract::{Query, State}, http::StatusCode, Json};
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
}

#[derive(Serialize)]
pub struct SaveResponse {
    pub saved:            bool,
    pub restart_required: bool,
}

// ── GET /api/v1/admin/settings ────────────────────────────────────────────────

pub async fn get_settings(
    State(state): State<SharedState>,
) -> Result<Json<SettingsResponse>, StatusCode> {
    let cfg = &state.config;

    let effective = AdminSettings {
        // Sampling
        sample_mode:                      Some(cfg.sampling.mode.as_str().to_string()),
        sample_line_count:                Some(cfg.sampling.line_count as u64),
        // Service
        run_mode:                         Some(cfg.service.run_mode.to_string()),
        poll_interval_secs:               Some(cfg.service.poll_interval_secs),
        concurrency:                      Some(cfg.service.concurrency as u64),
        ssh_timeout_secs:                 Some(cfg.service.ssh_timeout_secs),
        // Discovery
        remote_max_depth:                 Some(cfg.discovery.max_depth as u64),
        remote_max_files_per_target:      Some(cfg.discovery.max_files_per_target as u64),
        remote_find_patterns:             Some(cfg.discovery.find_patterns.join(",")),
        // Preprocessing
        preprocessing_enabled:            Some(cfg.preprocessing.enabled),
        preprocessing_agentic_threshold:  Some(cfg.preprocessing.agentic_threshold),
        preprocessing_max_schema_lines:   Some(cfg.preprocessing.max_schema_lines as u64),
        // Classification
        classification_enabled:           Some(cfg.classification.enabled),
        // Mask API key: show "***" when set, empty string when not set
        anthropic_api_key:                Some(
            if cfg.classification.api_key.is_empty() { String::new() } else { "***".to_string() }
        ),
        classification_model:             Some(cfg.classification.model.clone()),
        classification_signal_threshold:  Some(cfg.classification.signal_threshold),
        classification_max_per_cycle:     Some(cfg.classification.max_per_cycle as u64),
        classification_max_output_tokens: Some(cfg.classification.max_output_tokens as u64),
        classification_api_base_url:      Some(cfg.classification.api_base_url.clone()),
        classification_api_format:        Some(cfg.classification.api_format.clone()),
        // Notifications
        notification_enabled:             Some(cfg.notification.enabled),
        notification_severity_threshold:  Some(cfg.notification.severity_threshold.as_str().to_string()),
        slack_webhook_url:                Some(cfg.notification.slack_webhook_url.clone().unwrap_or_default()),
        webhook_url:                      Some(cfg.notification.webhook_url.clone().unwrap_or_default()),
        // Mask webhook secret the same way
        webhook_secret:                   Some(
            if cfg.notification.webhook_secret.is_some() { "***".to_string() } else { String::new() }
        ),
        // Logging
        log_level:                        Some(cfg.logging.level.clone()),
    };

    let has_overrides = state
        .repo
        .load_admin_settings()
        .await
        .ok()
        .flatten()
        .is_some();

    Ok(Json(SettingsResponse { settings: effective, has_overrides }))
}

// ── PUT /api/v1/admin/settings ────────────────────────────────────────────────

pub async fn put_settings(
    State(state): State<SharedState>,
    Json(mut incoming): Json<AdminSettings>,
) -> Result<Json<SaveResponse>, StatusCode> {
    // Load existing stored overrides so we can preserve masked fields that
    // the user left as "***" (meaning "don't change this value").
    let existing = state
        .repo
        .load_admin_settings()
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    // Preserve API key when the UI sends the placeholder back unchanged.
    if incoming.anthropic_api_key.as_deref() == Some("***") {
        incoming.anthropic_api_key = existing.anthropic_api_key;
    }
    // Same for webhook secret.
    if incoming.webhook_secret.as_deref() == Some("***") {
        incoming.webhook_secret = existing.webhook_secret;
    }

    if let Err(e) = state.repo.save_admin_settings(&incoming).await {
        error!(error = %e, "failed to save admin settings");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    info!("admin settings saved to MongoDB");
    Ok(Json(SaveResponse { saved: true, restart_required: true }))
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
