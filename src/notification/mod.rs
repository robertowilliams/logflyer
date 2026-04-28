use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value as JsonValue};
use sha2::Sha256;
use tracing::{error, info, warn};

use crate::config::NotificationConfig;
use crate::models::{ClassificationRecord, Severity};

type HmacSha256 = Hmac<Sha256>;

// ── Worker ────────────────────────────────────────────────────────────────────

pub struct NotificationWorker {
    config: NotificationConfig,
    http:   Client,
}

impl NotificationWorker {
    pub fn new(config: NotificationConfig) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    /// Fire notifications for a classification result.
    ///
    /// Only sends when:
    /// - `config.enabled = true`
    /// - `record.severity.level() >= config.severity_threshold.level()`
    ///
    /// Never panics; errors are logged and swallowed so the classification
    /// pipeline is never blocked by a failing webhook.
    pub async fn notify(&self, record: &ClassificationRecord) {
        if !self.config.enabled {
            return;
        }

        if record.severity.level() < self.config.severity_threshold.level() {
            return;
        }

        info!(
            target_id   = %record.target_id,
            sample_hash = %record.sample_hash,
            severity    = record.severity.as_str(),
            "firing notifications"
        );

        if let Some(url) = &self.config.slack_webhook_url {
            if let Err(e) = self.send_slack(url, record).await {
                error!(error = %e, "slack notification failed");
            }
        }

        if let Some(url) = &self.config.webhook_url {
            if let Err(e) = self.send_webhook(url, record).await {
                error!(error = %e, "webhook notification failed");
            }
        }

        if self.config.slack_webhook_url.is_none() && self.config.webhook_url.is_none() {
            warn!(
                "NOTIFICATION_ENABLED=true but neither SLACK_WEBHOOK_URL nor \
                 WEBHOOK_URL is set — no notifications sent"
            );
        }
    }

    // ── Slack ─────────────────────────────────────────────────────────────────

    async fn send_slack(
        &self,
        url:    &str,
        record: &ClassificationRecord,
    ) -> Result<(), String> {
        let severity_emoji = match record.severity {
            Severity::Critical => "🚨",
            Severity::Warning  => "⚠️",
            Severity::Info     => "ℹ️",
            Severity::Normal   => "✅",
        };

        let findings_text = if record.key_findings.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = record
                .key_findings
                .iter()
                .take(5)
                .map(|f| format!("• *{}* ×{} — _{}_", f.pattern, f.count, f.severity))
                .collect();
            format!("\n*Key findings:*\n{}", lines.join("\n"))
        };

        let recommendations_text = if record.recommendations.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = record
                .recommendations
                .iter()
                .take(3)
                .map(|r| format!("→ {r}"))
                .collect();
            format!("\n*Recommendations:*\n{}", lines.join("\n"))
        };

        let payload = json!({
            "blocks": [
                {
                    "type": "header",
                    "text": {
                        "type": "plain_text",
                        "text": format!("{severity_emoji} logflayer: {} finding on {}",
                            record.severity.as_str().to_uppercase(),
                            record.target_id),
                        "emoji": true
                    }
                },
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": format!(
                            "*Target:* `{}`\n*Summary:* {}{}{}\n*Confidence:* {}%  |  *Model:* `{}`",
                            record.target_id,
                            record.summary,
                            findings_text,
                            recommendations_text,
                            (record.confidence * 100.0).round() as u32,
                            record.model,
                        )
                    }
                },
                {
                    "type": "context",
                    "elements": [{
                        "type": "mrkdwn",
                        "text": format!("sample_hash: `{}`  |  {}", record.sample_hash, record.classified_at)
                    }]
                }
            ]
        });

        self.post_json(url, &payload, None).await
    }

    // ── Generic webhook ───────────────────────────────────────────────────────

    async fn send_webhook(
        &self,
        url:    &str,
        record: &ClassificationRecord,
    ) -> Result<(), String> {
        let payload = json!({
            "event":           "classification.alert",
            "severity":        record.severity.as_str(),
            "target_id":       record.target_id,
            "sample_hash":     record.sample_hash,
            "classified_at":   record.classified_at.to_rfc3339_string(),
            "summary":         record.summary,
            "categories":      record.categories,
            "key_findings":    record.key_findings.iter().map(|f| json!({
                "pattern":  f.pattern,
                "count":    f.count,
                "severity": f.severity,
                "example":  f.example,
            })).collect::<Vec<_>>(),
            "recommendations": record.recommendations,
            "confidence":      record.confidence,
            "model":           record.model,
            "input_tokens":    record.input_tokens,
            "output_tokens":   record.output_tokens,
        });

        let signature = self
            .config
            .webhook_secret
            .as_deref()
            .map(|secret| compute_hmac_sha256(secret, payload.to_string().as_bytes()));

        self.post_json(url, &payload, signature.as_deref()).await
    }

    // ── Shared HTTP send ──────────────────────────────────────────────────────

    async fn post_json(
        &self,
        url:       &str,
        payload:   &JsonValue,
        signature: Option<&str>,
    ) -> Result<(), String> {
        let mut req = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .json(payload);

        if let Some(sig) = signature {
            req = req.header("X-Logflayer-Signature", format!("sha256={sig}"));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {status}: {body}"));
        }

        Ok(())
    }
}

// ── HMAC-SHA256 helper ────────────────────────────────────────────────────────

fn compute_hmac_sha256(secret: &str, data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}
