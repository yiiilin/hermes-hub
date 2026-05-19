pub mod app_config;
pub mod db;
pub mod hermes;
pub mod http;
pub mod security;
pub mod session {
    pub mod store;
}
pub mod domain {
    pub mod invite;
    pub mod user;
}

use axum::{routing::get, Json, Router};
use serde::Serialize;
use session::store::SessionStore;

pub use app_config::AppConfig;

/// Shared application state for HTTP handlers.
///
/// The store is intentionally in-memory for the early MVP skeleton. The public
/// methods are written so a SQLx-backed implementation can replace it later.
#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub store: SessionStore,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// Build the backend HTTP router.
pub fn build_router(config: AppConfig) -> Router {
    let state = AppState {
        config,
        store: SessionStore::default(),
    };

    Router::new()
        .route("/health", get(health))
        .merge(http::router())
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}
