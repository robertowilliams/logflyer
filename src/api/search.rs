//! `POST /api/v1/search` — nearest-neighbour search over the embedding
//! collections.
//!
//! The vector writer has been persisting `content_embeddings` and
//! `behavioral_embeddings` since Phase 6B, but nothing ever read them. This is
//! the read side.
//!
//! # What it searches
//!
//! Embeddings are keyed per **sample** (`embedding_id` derives from
//! `(sample_hash, kind, model)`), so results are samples, not entities or log
//! lines. The question it answers is "which samples resemble this one" — the
//! behavioural-clustering case the plan described as "find all agent runs with a
//! similar tool-use pattern".
//!
//! # Two ways to ask
//!
//! ```jsonc
//! // by example — look up this sample's own vector and find its neighbours
//! { "sample_hash": "9cf60df0…", "kind": "behavioral", "limit": 5 }
//!
//! // by raw vector — for a caller that computed its own
//! { "vector": [0.1, 0.2, …], "kind": "behavioral" }
//! ```
//!
//! `kind` defaults to `behavioral`, because behavioral vectors are computed
//! locally and always present, whereas content embeddings need an API key and
//! are off by default.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::SharedState;
use crate::embedding::EmbeddingKind;
use crate::repository::{DEFAULT_LIMIT, MAX_LIMIT};

// ─── Request ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    /// Search by example: use this sample's own embedding as the query.
    /// Mutually exclusive with `vector` and `task_id`.
    #[serde(default)]
    pub sample_hash: Option<String>,
    /// Search by example for `kind: "task"` — find tasks resembling this one.
    ///
    /// Separate from `sample_hash` because task embeddings are keyed on
    /// `task_id`; conflating them would silently look up the wrong field and
    /// report "no embedding" for a task that has one.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Search by an explicit query vector.
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// `content` or `behavioral`. Defaults to `behavioral`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Restrict results to one sampling target.
    #[serde(default)]
    pub target_id: Option<String>,
    /// Include the query's own sample in the results. Defaults to `false`, since
    /// it always scores `1.0` and crowds out the answer you asked for. Only
    /// meaningful with `sample_hash`.
    #[serde(default)]
    pub include_self: bool,
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

// ─── Handler ──────────────────────────────────────────────────────────────────

/// `POST /api/v1/search`
///
/// `400` when neither `sample_hash` nor `vector` is given, when both are, when
/// `kind` is unrecognised, or when the named sample has no embedding of that kind
/// — that last one is a common and confusing case (content embeddings are off by
/// default), so it gets an explanatory message rather than an empty result.
pub async fn search(
    State(s): State<SharedState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let kind = match req.kind.as_deref() {
        None | Some("behavioral") => EmbeddingKind::Behavioral,
        Some("content") => EmbeddingKind::Content,
        Some("task") => EmbeddingKind::Task,
        Some(other) => {
            return Err(bad_request(format!(
                "unknown kind {other:?}; expected \"content\", \"behavioral\" or \"task\""
            )))
        }
    };

    // `task_id` is the search-by-example key for task search, `sample_hash` for
    // the sample-scoped kinds. Normalise them into one "by example" id after
    // checking the caller did not mix incompatible forms.
    if req.task_id.is_some() && req.sample_hash.is_some() {
        return Err(bad_request(
            "give either `task_id` or `sample_hash`, not both".to_string(),
        ));
    }
    if req.task_id.is_some() && kind != EmbeddingKind::Task {
        return Err(bad_request(format!(
            "`task_id` only applies to kind \"task\"; got {:?}",
            kind_str(kind),
        )));
    }
    if req.sample_hash.is_some() && kind == EmbeddingKind::Task {
        return Err(bad_request(
            "kind \"task\" searches by `task_id`, not `sample_hash` — task \
             embeddings are keyed per task, not per sample"
                .to_string(),
        ));
    }
    let by_example = req.task_id.as_deref().or(req.sample_hash.as_deref());

    // Resolve the query vector, from whichever form the caller used.
    let (query, self_hash) = match (by_example, req.vector.as_ref()) {
        (Some(_), Some(_)) => {
            return Err(bad_request(
                "give either an id to search by example or a `vector`, not both".to_string(),
            ))
        }
        (None, None) => {
            return Err(bad_request(
                "one of `task_id`, `sample_hash` or `vector` is required".to_string(),
            ))
        }
        (Some(hash), None) => {
            match s.repo.fetch_embedding_vector(hash, kind).await {
                Ok(Some(v)) => (v, Some(hash.to_string())),
                Ok(None) => {
                    return Err(bad_request(format!(
                        "sample {hash} has no {} embedding. Content embeddings require \
                         EMBEDDING_ENABLED=true and an API key; behavioral ones are written \
                         whenever VECTOR_WRITER_ENABLED=true.",
                        kind_str(kind),
                    )))
                }
                Err(e) => return Err(internal(e.to_string())),
            }
        }
        (None, Some(v)) => {
            if v.is_empty() {
                return Err(bad_request("`vector` must not be empty".to_string()));
            }
            (v.clone(), None)
        }
    };

    if req.limit > MAX_LIMIT {
        return Err(bad_request(format!(
            "limit {} exceeds the maximum of {MAX_LIMIT}",
            req.limit
        )));
    }

    // Drop the query's own sample unless asked to keep it.
    let exclude = match (&self_hash, req.include_self) {
        (Some(hash), false) => Some(hash.as_str()),
        _ => None,
    };

    match s
        .repo
        .search_embeddings(kind, &query, req.limit, req.target_id.as_deref(), exclude)
        .await
    {
        Ok(result) => Ok(Json(json!({
            "kind":            kind_str(kind),
            "query_dimensions": query.len(),
            "hits":            result.hits,
            "hit_count":       result.hits.len(),
            // How many candidates were comparable, and how many were not. All
            // skipped with nothing scored means the query's dimensionality did
            // not match what is stored — 36 for behavioral, 1536 for content.
            "scored":          result.scored,
            "skipped":         result.skipped,
            "truncated":       result.truncated,
        }))),
        Err(e) => Err(internal(e.to_string())),
    }
}

fn kind_str(kind: EmbeddingKind) -> &'static str {
    kind.as_str()
}

fn bad_request(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn internal(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
}
