//! Graph traversal endpoints over the `entity_edges` collection.
//!
//! | Route                                    | Question it answers            |
//! |------------------------------------------|--------------------------------|
//! | `GET /api/v1/graph/downstream/:entity_id`| What did this entity cause?    |
//! | `GET /api/v1/graph/upstream/:entity_id`  | What caused this entity?       |
//! | `GET /api/v1/graph/path?from=&to=`       | How is A connected to B?       |
//!
//! Before these existed, `entity_edges` was only paged-listable — you could see
//! that edges had been written but not follow them, which is most of the reason
//! to keep a graph store in the first place.
//!
//! Every response carries `edges` and `entities` arrays shaped to match the
//! props of the frontend's `RelationGraph` component, so a traversal result can
//! be rendered without client-side reshaping.
//!
//! `depth` is clamped to [`MAX_DEPTH`]; traversals that would exceed
//! [`MAX_NODES`] stop early and set `truncated: true` rather than failing, since
//! a partial graph is more useful than an error.
//!
//! [`MAX_DEPTH`]: crate::repository::MAX_DEPTH
//! [`MAX_NODES`]: crate::repository::MAX_NODES

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;
use crate::error::AppError;
use crate::repository::{Direction, PathOutcome};

// ─── Query types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TraverseQuery {
    /// Hops to walk. Clamped to `1..=MAX_DEPTH`; defaults to 2.
    #[serde(default = "default_depth")]
    depth: u32,
}

fn default_depth() -> u32 {
    2
}

#[derive(Deserialize)]
pub struct PathQuery {
    from: String,
    to: String,
    /// Maximum hops to search. Clamped to `1..=MAX_DEPTH`; defaults to 6.
    #[serde(default = "default_path_depth")]
    max_depth: u32,
}

fn default_path_depth() -> u32 {
    6
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/v1/graph/downstream/:entity_id?depth=N`
pub async fn downstream(
    State(s): State<SharedState>,
    Path(entity_id): Path<String>,
    Query(q): Query<TraverseQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    traverse(s, entity_id, Direction::Downstream, q.depth).await
}

/// `GET /api/v1/graph/upstream/:entity_id?depth=N`
pub async fn upstream(
    State(s): State<SharedState>,
    Path(entity_id): Path<String>,
    Query(q): Query<TraverseQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    traverse(s, entity_id, Direction::Upstream, q.depth).await
}

async fn traverse(
    s: SharedState,
    entity_id: String,
    direction: Direction,
    depth: u32,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match s.repo.traverse_graph(&entity_id, direction, depth).await {
        Ok(result) => Ok(Json(json!({
            "root":          result.root,
            "direction":     result.direction,
            "depth_reached": result.depth_reached,
            "edges":         result.edges,
            "entities":      result.entities,
            "node_ids":      result.node_ids,
            "node_count":    result.node_ids.len(),
            "edge_count":    result.edges.len(),
            "truncated":     result.truncated,
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

/// `GET /api/v1/graph/path?from=:id&to=:id&max_depth=N`
///
/// Responds `200` with `found: false` when the target is unreachable — an
/// unreachable pair is a legitimate answer, not a client error.
///
/// `truncated: true` alongside `found: false` means the search hit a budget
/// before it could finish, so the pair may in fact be connected.  The two are
/// reported separately because "there is no path" and "we stopped looking" are
/// different claims.
pub async fn path(
    State(s): State<SharedState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Echo back the normalised ids so the response matches on both branches.
    let from = strip_prefix(&q.from);
    let to = strip_prefix(&q.to);

    match s.repo.graph_path(&q.from, &q.to, q.max_depth).await {
        Ok(PathOutcome::Found(result)) => Ok(Json(json!({
            "found":     true,
            "truncated": result.truncated,
            "from":      result.from,
            "to":        result.to,
            "hops":      result.hops,
            "hop_count": result.hops.len(),
            "edges":     result.edges,
            "entities":  result.entities,
            "node_ids":  result.node_ids,
        }))),
        Ok(outcome @ (PathOutcome::NotFound | PathOutcome::Truncated)) => Ok(Json(json!({
            "found":     false,
            "truncated": matches!(outcome, PathOutcome::Truncated),
            "from":      from,
            "to":        to,
            "hops":      [],
            "hop_count": 0,
            "edges":     [],
            "entities":  [],
            "node_ids":  [],
        }))),
        // An empty `from` / `to` is the caller's mistake, not a server fault.
        Err(AppError::Validation(msg)) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": msg })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

/// Mirror of the repository's URI normalisation, for echoing ids back.
fn strip_prefix(id: &str) -> &str {
    id.strip_prefix("ug:entity:").unwrap_or(id)
}
