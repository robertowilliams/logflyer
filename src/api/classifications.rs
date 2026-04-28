use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use tracing::error;

use super::SharedState;

// ─── Query params ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListParams {
    pub target_id: Option<String>,
    pub page:      Option<u64>,
    pub limit:     Option<i64>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// GET /api/v1/classifications
pub async fn list(
    State(state): State<SharedState>,
    Query(params): Query<ListParams>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let page  = params.page.unwrap_or(0);

    match state
        .repo
        .fetch_classifications_page(params.target_id.as_deref(), limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(serde_json::json!({
            "records": records,
            "total":   total,
            "page":    page,
            "limit":   limit,
        }))),
        Err(e) => {
            error!(error = %e, "fetch_classifications_page failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// GET /api/v1/classifications/:hash
pub async fn get_one(
    State(state): State<SharedState>,
    Path(hash): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.repo.find_classification_by_hash(&hash).await {
        Ok(Some(record)) => Ok(Json(record)),
        Ok(None)         => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            error!(error = %e, hash = %hash, "get_one classification failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
