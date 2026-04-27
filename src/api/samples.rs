use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

#[derive(Deserialize)]
pub struct SamplesQuery {
    #[serde(default)]
    target_id: Option<String>,
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
    Query(q): Query<SamplesQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };

    match s
        .repo
        .fetch_samples_page(q.target_id.as_deref(), q.limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total": total,
            "page": page + 1,
            "limit": q.limit,
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

pub async fn collections(
    State(s): State<SharedState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.list_sample_collections().await {
        Ok(names) => Ok(Json(json!({ "collections": names }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}
