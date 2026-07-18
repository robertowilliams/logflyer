//! `GET /api/v1/prov` — paginated read of W3C PROV-O triples produced by
//! the Phase 6 graph writer.
//!
//! Triples are stored in the `prov_relations` collection with
//! `(sample_hash, subject, predicate, object)` as the composite identity.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

#[derive(Deserialize)]
pub struct ProvQuery {
    /// Limit triples to a single sample.
    #[serde(default)]
    sample_hash: Option<String>,
    /// Filter by subject URI (e.g. `"ug:entity:..."`).
    #[serde(default)]
    subject: Option<String>,
    /// Filter by serialised predicate (camelCase: `wasGeneratedBy`,
    /// `wasAttributedTo`, `wasDerivedFrom`, `used`, `actedOnBehalfOf`).
    #[serde(default)]
    predicate: Option<String>,
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
    Query(q): Query<ProvQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };
    match s
        .repo
        .fetch_prov_page(
            q.sample_hash.as_deref(),
            q.subject.as_deref(),
            q.predicate.as_deref(),
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
