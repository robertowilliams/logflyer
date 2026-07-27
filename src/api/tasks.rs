//! Task and actor endpoints (Stages 11–13).
//!
//! | Route | Question |
//! |---|---|
//! | `GET /api/v1/tasks` | what units of work do we have? |
//! | `GET /api/v1/tasks/:task_id` | what was this task, and what was it for? |
//! | `GET /api/v1/tasks/:task_id/graph` | **the audit payload** — the whole interaction graph |
//! | `GET /api/v1/actors` | which agents, skills and resources exist? |
//!
//! `/tasks/:id/graph` is the second half of the loop that `POST /api/v1/search`
//! with `kind: "task"` begins: search by what a task was *for*, then open the
//! graph of what actually happened and audit the reasoning.
//!
//! Unlike the `/graph/{downstream,upstream}` endpoints, this one does not
//! traverse. The task *is* the boundary, so everything belonging to its samples
//! is in scope — events, edges, and the participants those edges point at.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;

// ─── Query types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TasksQuery {
    #[serde(default)]
    target_id: Option<String>,
    /// Exclude tasks whose boundary came from the sample fallback.
    ///
    /// Those are not task boundaries in any meaningful sense — just
    /// one-task-per-sample placeholders for logs carrying no correlation key —
    /// so a caller browsing real units of work usually wants them gone.
    #[serde(default)]
    real_boundaries_only: bool,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    page: u64,
}

#[derive(Deserialize)]
pub struct ActorsQuery {
    /// `agent`, `skill` or `resource`.
    #[serde(default)]
    kind: Option<String>,
    /// Restrict to participants in one task — "who worked on this".
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    page: u64,
}

fn default_limit() -> i64 {
    50
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/v1/tasks`
pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<TasksQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };
    match s
        .repo
        .fetch_tasks_page(q.target_id.as_deref(), q.real_boundaries_only, q.limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total":   total,
            "page":    page + 1,
            "limit":   q.limit,
        }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

/// `GET /api/v1/tasks/:task_id`
pub async fn get_one(
    State(s): State<SharedState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.fetch_task(&task_id).await {
        Ok(Some(task)) => Ok(Json(json!({ "task": task }))),
        Ok(None) => Err(not_found(format!("no task with id {task_id}"))),
        Err(e) => Err(internal(e.to_string())),
    }
}

/// `GET /api/v1/tasks/:task_id/graph`
///
/// The audit payload: every event across the task's samples, every edge between
/// them, and the actors those edges reach.
///
/// `truncated` means the task spans more samples than one response will assemble;
/// `sample_hashes` then lists the subset actually included, so the caller knows
/// what it is looking at rather than assuming completeness.
pub async fn graph(
    State(s): State<SharedState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.fetch_task_graph(&task_id).await {
        Ok(Some(g)) => Ok(Json(json!({
            "task":           g.task,
            "entities":       g.entities,
            "relations":      g.relations,
            "actors":         g.actors,
            "sample_hashes":  g.sample_hashes,
            "entity_count":   g.entities.len(),
            "relation_count": g.relations.len(),
            "actor_count":    g.actors.len(),
            "sample_count":   g.sample_hashes.len(),
            "truncated":      g.truncated,
        }))),
        Ok(None) => Err(not_found(format!("no task with id {task_id}"))),
        Err(e) => Err(internal(e.to_string())),
    }
}

/// `GET /api/v1/actors`
pub async fn actors(
    State(s): State<SharedState>,
    Query(q): Query<ActorsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if let Some(kind) = q.kind.as_deref() {
        if !matches!(kind, "agent" | "skill" | "resource") {
            return Err(bad_request(format!(
                "unknown kind {kind:?}; expected \"agent\", \"skill\" or \"resource\""
            )));
        }
    }

    let page = if q.page == 0 { 0 } else { q.page - 1 };
    match s
        .repo
        .fetch_actors_page(q.kind.as_deref(), q.task_id.as_deref(), q.limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total":   total,
            "page":    page + 1,
            "limit":   q.limit,
        }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

// ─── Error helpers ────────────────────────────────────────────────────────────

fn bad_request(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn not_found(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message })))
}

fn internal(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
}
