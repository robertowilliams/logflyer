use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::SharedState;

pub async fn list(State(s): State<SharedState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.list_all_targets().await {
        Ok(docs) => {
            let total = docs.len();
            Ok(Json(json!({ "targets": docs, "total": total })))
        }
        Err(e) => Err(internal(e.to_string())),
    }
}

pub async fn create(
    State(s): State<SharedState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.create_target(body).await {
        Ok(doc) => Ok(Json(json!({ "target": doc }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

pub async fn update(
    State(s): State<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.update_target(&id, body).await {
        Ok(doc) => Ok(Json(json!({ "target": doc }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

pub async fn delete_one(
    State(s): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.delete_target(&id).await {
        Ok(_) => Ok(Json(json!({ "deleted": true, "id": id }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

pub async fn toggle(
    State(s): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.toggle_target_status(&id).await {
        Ok(new_status) => Ok(Json(json!({ "id": id, "status": new_status }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

fn internal(msg: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
}
