//! Content embedding — truncates raw log text and calls an OpenAI-compatible
//! embeddings endpoint to produce a dense semantic vector.
//!
//! The text sent to the API is the raw log content, truncated to
//! [`EmbeddingConfig::max_text_chars`] Unicode code points (~4 chars/token).
//! If the content is empty the function returns `None` without making an API call.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::AppError;

// ─── Wire types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    input:  &'a str,
    model:  &'a str,
    /// Omitted when zero — lets the provider use the model's native dimensionality.
    #[serde(skip_serializing_if = "is_zero")]
    dimensions: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool { *n == 0 }

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingObject>,
}

#[derive(Deserialize)]
struct EmbeddingObject {
    embedding: Vec<f32>,
}

// ─── Text extraction ──────────────────────────────────────────────────────────

/// Truncate `content` to at most `max_chars` Unicode code points.
///
/// Uses an iterator over `chars()` so the result is always valid UTF-8
/// regardless of multi-byte boundaries.  Returns an empty `String` when
/// `content` is empty, not `None`, so the caller can decide whether to skip.
pub fn truncate(content: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    content.chars().take(max_chars).collect()
}

// ─── HTTP client ─────────────────────────────────────────────────────────────

/// Call the OpenAI-compatible `/v1/embeddings` endpoint.
///
/// Returns `None` when `text` is empty (no API call made).
/// Returns an error on HTTP failures or unexpected response shapes.
///
/// # Arguments
/// * `http`       – shared reqwest client (constructed once by [`super::EmbeddingWorker`])
/// * `api_key`    – Bearer token
/// * `base_url`   – provider base URL; defaults to `https://api.openai.com`
/// * `model`      – model name (e.g. `"text-embedding-3-small"`)
/// * `dimensions` – requested output dimensions; `0` lets the provider choose
/// * `text`       – text to embed (should already be truncated)
pub async fn embed(
    http:       &Client,
    api_key:    &str,
    base_url:   &str,
    model:      &str,
    dimensions: u32,
    text:       &str,
) -> Result<Option<Vec<f32>>, AppError> {
    if text.is_empty() {
        return Ok(None);
    }

    let base = if base_url.is_empty() {
        "https://api.openai.com"
    } else {
        base_url.trim_end_matches('/')
    };
    let url = format!("{base}/v1/embeddings");

    let body = EmbeddingsRequest { input: text, model, dimensions };

    let resp = http
        .post(&url)
        .header("Authorization",  format!("Bearer {api_key}"))
        .header("Content-Type",   "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Embedding(format!("embeddings request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Embedding(format!(
            "embeddings API returned {status}: {body}"
        )));
    }

    let parsed: EmbeddingsResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Embedding(format!("failed to parse embeddings response: {e}")))?;

    let vector = parsed
        .data
        .into_iter()
        .next()
        .map(|obj| obj.embedding)
        .ok_or_else(|| AppError::Embedding("embeddings response contained no data".to_string()))?;

    if vector.is_empty() {
        return Err(AppError::Embedding("embeddings response returned an empty vector".to_string()));
    }

    Ok(Some(vector))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate() ────────────────────────────────────────────────────────────

    #[test]
    fn truncate_empty_returns_empty() {
        assert_eq!(truncate("", 100), "");
    }

    #[test]
    fn truncate_short_string_unchanged() {
        let s = "hello world";
        assert_eq!(truncate(s, 1000), s);
    }

    #[test]
    fn truncate_exactly_at_limit_unchanged() {
        let s = "abcde";
        assert_eq!(truncate(s, 5), "abcde");
    }

    #[test]
    fn truncate_longer_string_cuts_at_limit() {
        let s = "hello world";
        assert_eq!(truncate(s, 5), "hello");
    }

    #[test]
    fn truncate_zero_limit_returns_empty() {
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn truncate_handles_multibyte_unicode() {
        // "😀" is 4 bytes; taking 2 chars should give 2 emoji, not crash
        let s = "😀😀😀😀";
        let t = truncate(s, 2);
        assert_eq!(t, "😀😀");
        // Must be valid UTF-8 (String invariant)
        assert_eq!(t.chars().count(), 2);
    }

    #[test]
    fn truncate_cyrillic_multibyte() {
        let s = "Привет мир"; // 10 chars, each 2 bytes
        let t = truncate(s, 7);
        assert_eq!(t, "Привет ");
        assert_eq!(t.chars().count(), 7);
    }

    #[test]
    fn truncate_preserves_ascii_log_line() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let t = truncate(line, 20);
        assert_eq!(t.len(), 20);
        // Slice must be valid UTF-8 (all ASCII here)
        assert!(t.is_ascii());
    }

    #[test]
    fn truncate_large_limit_returns_full_string() {
        let s = "log line content here";
        assert_eq!(truncate(s, usize::MAX), s);
    }

    // ── Wire type serialisation ───────────────────────────────────────────────

    #[test]
    fn embeddings_request_omits_dimensions_when_zero() {
        let req = EmbeddingsRequest {
            input:      "test",
            model:      "text-embedding-3-small",
            dimensions: 0,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(!json.as_object().unwrap().contains_key("dimensions"),
                "dimensions field should be omitted when zero");
    }

    #[test]
    fn embeddings_request_includes_dimensions_when_nonzero() {
        let req = EmbeddingsRequest {
            input:      "test",
            model:      "text-embedding-3-small",
            dimensions: 1536,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["dimensions"], 1536);
    }

    #[test]
    fn embeddings_response_deserialises_correctly() {
        let raw = r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0,"object":"embedding"}],"model":"text-embedding-3-small","usage":{"prompt_tokens":5,"total_tokens":5}}"#;
        let resp: EmbeddingsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].embedding, vec![0.1f32, 0.2, 0.3]);
    }

    #[test]
    fn embeddings_response_handles_multiple_objects() {
        // We only consume the first data object; the parser must not panic on extras.
        let raw = r#"{"data":[{"embedding":[1.0]},{"embedding":[2.0]}]}"#;
        let resp: EmbeddingsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.data.len(), 2);
    }
}
