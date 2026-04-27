use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{delete, get, patch, post, put};
use axum::{extract::State, Json, Router};
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::config::AppConfig;
use crate::repository::MongoRepository;

pub mod logs;
pub mod samples;
pub mod targets;
pub mod tracking;

// ─── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ApiState {
    pub repo: MongoRepository,
    pub config: AppConfig,
}

pub type SharedState = Arc<ApiState>;

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn build_router(state: ApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/targets", get(targets::list).post(targets::create))
        .route(
            "/api/v1/targets/:id",
            put(targets::update).delete(targets::delete_one),
        )
        .route("/api/v1/targets/:id/toggle", patch(targets::toggle))
        .route("/api/v1/logs", get(logs::list))
        .route("/api/v1/tracking", get(tracking::list))
        .route("/api/v1/samples", get(samples::list))
        .route("/api/v1/samples/collections", get(samples::collections))
        .layer(CorsLayer::permissive())
        .with_state(shared)
}

// ─── Health endpoint ──────────────────────────────────────────────────────────

async fn health_handler(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let ok = state.repo.ping().await.is_ok();
    Json(serde_json::json!({
        "status": if ok { "healthy" } else { "degraded" },
        "mongodb": if ok { "connected" } else { "unreachable" },
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ─── Server startup ───────────────────────────────────────────────────────────

pub async fn start(state: ApiState, port: u16) {
    let app = build_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(port, "API server listening on :{}", port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind API port");

    axum::serve(listener, app)
        .await
        .expect("API server crashed");
}
