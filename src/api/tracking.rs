use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

#[derive(Deserialize)]
pub struct TrackingQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    page: u64,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    level: Option<String>,
}

fn default_limit() -> i64 {
    50
}

pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<TrackingQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };

    match s
        .repo
        .fetch_tracking_logs(q.limit, page, q.search.as_deref(), q.level.as_deref())
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
