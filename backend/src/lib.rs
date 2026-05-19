pub mod app_config;

use axum::{routing::get, Json, Router};
use serde::Serialize;

pub use app_config::AppConfig;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// 构建后端 HTTP 路由。当前先暴露健康检查，后续任务会挂载认证、代理和管理 API。
pub fn build_router(_config: AppConfig) -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}
