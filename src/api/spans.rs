//! `GET /api/v1/spans` — paginated read of OTel-compatible span records
//! produced by the Phase 6 graph writer.
//!
//! Spans are stored in the `otel_spans` collection with
//! `(trace_id, span_id)` as the composite identity, sortable by
//! `start_time_unix_nano` to reconstruct the trace timeline.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

#[derive(Deserialize)]
pub struct SpansQuery {
    /// Limit spans to a single sample.
    #[serde(default)]
    sample_hash: Option<String>,
    /// Limit spans to a single trace (32-hex-char OTel trace ID).
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    page: u64,
}

fn default_limit() -> i64 {
    50
}

pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<SpansQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };
    match s
        .repo
        .fetch_spans_page(
            q.sample_hash.as_deref(),
            q.trace_id.as_deref(),
            q.limit,
            page,
        )
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total":   total,
            "page":    page + 1,
            "limit":   q.limit,
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}
