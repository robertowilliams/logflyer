//! `GET /api/v1/entities/:entity_id` — resolve a single [`EntityRecord`].
//!
//! The UpsideGate views hold entity *references* in several places where the
//! full record is not loaded: PROV triples carry `ug:entity:{id}` URIs, and
//! relation edges carry bare `source_entity_id` / `target_entity_id`.  When the
//! referenced entity is not in the currently-selected sample's
//! `metadata.entities` array — which happens as soon as the user queries across
//! samples — the frontend has nothing to render but the id.
//!
//! This endpoint closes that gap.  It accepts either spelling of the id, so
//! callers do not have to strip the URI prefix themselves.
//!
//! [`EntityRecord`]: crate::models::EntityRecord

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::SharedState;

/// `GET /api/v1/entities/:entity_id`
///
/// Responds `404` when no sample contains an entity with that id.
pub async fn get_one(
    State(s): State<SharedState>,
    Path(entity_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.fetch_entity_by_id(&entity_id).await {
        Ok(Some(entity)) => Ok(Json(json!({ "entity": entity }))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no entity with id {entity_id}") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}
