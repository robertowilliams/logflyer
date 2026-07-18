use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

#[derive(Deserialize)]
pub struct MetadataQuery {
    #[serde(default)]
    target_id: Option<String>,
    /// Filter to samples that are worth classifying (agentic signal detected).
    #[serde(default)]
    worth_classifying: Option<bool>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    page: u64,
}

fn default_limit() -> i64 {
    50
}

/// `GET /api/v1/metadata` — paginated list of preprocessed sample metadata.
pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<MetadataQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };

    match s
        .repo
        .fetch_metadata_page(
            q.target_id.as_deref(),
            q.worth_classifying,
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

/// `GET /api/v1/metadata/:hash` — single metadata document by sample hash.
pub async fn get_one(
    State(s): State<SharedState>,
    Path(hash): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.fetch_metadata_by_hash(&hash).await {
        Ok(Some(meta)) => Ok(Json(json!({ "metadata": meta }))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no metadata for hash {hash}") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}
