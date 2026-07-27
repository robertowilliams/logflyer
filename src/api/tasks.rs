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

/// Ceiling on rows per page.
///
/// Matches `repository::MAX_LIMIT` on `/search`, which enforces its own ceiling
/// twice over. These two endpoints previously enforced none at all, and they are
/// the ones backed by a `count_documents` plus a sort the indexes cannot always
/// serve — so they were the cheapest in the API to abuse.
const MAX_PAGE_LIMIT: i64 = 500;

/// Clamp a caller-supplied `limit` into something Mongo will honour as written.
///
/// Two values are actively dangerous rather than merely large:
///
/// * `limit=0` — Mongo's find command reads zero as **no limit**, so the
///   friendliest-looking value in the API returned the entire collection.
/// * `limit=-1` — flows into `skip(page * limit as u64)`, where `3 * u64::MAX`
///   panics in a debug build and wraps in release.
fn clamp_limit(limit: i64) -> i64 {
    limit.clamp(1, MAX_PAGE_LIMIT)
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/v1/tasks`
pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<TasksQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let page = if q.page == 0 { 0 } else { q.page - 1 };
    let limit = clamp_limit(q.limit);
    match s
        .repo
        .fetch_tasks_page(q.target_id.as_deref(), q.real_boundaries_only, limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total":   total,
            "page":    page + 1,
            "limit":   limit,
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
    let limit = clamp_limit(q.limit);
    match s
        .repo
        .fetch_actors_page(q.kind.as_deref(), q.task_id.as_deref(), limit, page)
        .await
    {
        Ok((records, total)) => Ok(Json(json!({
            "records": records,
            "total":   total,
            "page":    page + 1,
            "limit":   limit,
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_does_not_mean_unlimited() {
        // Mongo's find command reads `limit: 0` as *no limit*, so the
        // friendliest-looking value in the API returned the whole collection.
        assert_eq!(clamp_limit(0), 1);
    }

    #[test]
    fn a_negative_limit_cannot_reach_the_skip_calculation() {
        // `skip(page * limit as u64)` turns -1 into u64::MAX, and `3 * u64::MAX`
        // panics in a debug build.
        assert_eq!(clamp_limit(-1), 1);
        assert_eq!(clamp_limit(i64::MIN), 1);
    }

    #[test]
    fn a_huge_limit_is_capped() {
        assert_eq!(clamp_limit(1_000_000), MAX_PAGE_LIMIT);
        assert_eq!(clamp_limit(i64::MAX), MAX_PAGE_LIMIT);
    }

    #[test]
    fn ordinary_limits_pass_through_untouched() {
        assert_eq!(clamp_limit(1), 1);
        assert_eq!(clamp_limit(default_limit()), 50);
        assert_eq!(clamp_limit(MAX_PAGE_LIMIT), MAX_PAGE_LIMIT);
    }
}
