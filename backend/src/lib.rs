pub mod app_config;
pub mod channel;
pub mod db;
pub mod hermes;
pub mod http;
pub mod llm_proxy;
pub mod model_config;
pub mod model_registry;
pub mod security;
pub mod session {
    pub mod store;
}
pub mod domain {
    pub mod invite;
    pub mod user;
}

use axum::{routing::get, Json, Router};
use channel::service::ChannelStore;
use hermes::{
    docker_provisioner::{DockerProvisioner, DockerProvisionerConfig, NoopDockerRuntime},
    proxy_client::{DynHermesProxyClient, InMemoryHermesProxyClient, ReqwestHermesProxyClient},
};
use llm_proxy::{DynLlmProviderClient, InMemoryLlmProviderClient, ReqwestLlmProviderClient};
use model_config::ModelRegistry;
use serde::Serialize;
use session::store::SessionStore;
use std::sync::Arc;
use thiserror::Error;
use tower_http::services::{ServeDir, ServeFile};

pub use app_config::AppConfig;

/// Shared application state for HTTP handlers.
///
/// 测试可以注入内存 adapter；`build_router_from_config` 在真实启动路径中注入
/// reqwest/Docker CLI adapter，避免生产请求落到 mock。
#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub store: SessionStore,
    pub channel_store: ChannelStore,
    pub hermes_proxy: DynHermesProxyClient,
    pub model_registry: ModelRegistry,
    pub llm_provider: DynLlmProviderClient,
    pub docker_provisioner: DockerProvisioner,
}

#[derive(Debug, Error)]
pub enum AppInitError {
    #[error("database connection failed")]
    Database(#[from] sqlx::Error),
    #[error("secret master key is required when DATABASE_URL is set")]
    MissingSecretMasterKey,
    #[error("secret master key is invalid")]
    InvalidSecretMasterKey(#[from] security::crypto::SecretCipherError),
    #[error("model registry initialization failed")]
    ModelRegistry(#[from] model_config::ModelRegistryError),
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// Build the backend HTTP router.
pub fn build_router(config: AppConfig) -> Router {
    let docker_provisioner = DockerProvisioner::new_with_runtime(
        docker_config_from_app(&config, config.initial_model_config.default_model.clone()),
        Arc::new(NoopDockerRuntime),
    );
    let state = AppState {
        model_registry: ModelRegistry::new(config.initial_model_config.clone()),
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        hermes_proxy: InMemoryHermesProxyClient::default().shared(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        docker_provisioner,
    };

    build_router_with_state(state)
}

/// 根据运行时配置构建 Router；存在 DATABASE_URL 时启用 PostgreSQL 后端。
pub async fn build_router_from_config(config: AppConfig) -> Result<Router, AppInitError> {
    let docker_provisioner = DockerProvisioner::new(docker_config_from_app(
        &config,
        config.initial_model_config.default_model.clone(),
    ));

    let Some(database_url) = config.database_url.clone() else {
        let state = AppState {
            model_registry: ModelRegistry::new(config.initial_model_config.clone()),
            config,
            store: SessionStore::default(),
            channel_store: ChannelStore::default(),
            hermes_proxy: ReqwestHermesProxyClient::default().shared(),
            llm_provider: ReqwestLlmProviderClient::default().shared(),
            docker_provisioner,
        };
        return Ok(build_router_with_state(state));
    };
    let secret_master_key = config
        .secret_master_key
        .clone()
        .ok_or(AppInitError::MissingSecretMasterKey)?;
    let cipher = security::crypto::SecretCipher::from_master_key(&secret_master_key)?;
    let pool = db::connect(&database_url).await?;
    db::migrations::run_migrations(&pool).await?;
    let model_registry = ModelRegistry::postgres(
        pool.clone(),
        cipher.clone(),
        config.initial_model_config.clone(),
    )
    .await?;
    let state = AppState {
        config,
        store: SessionStore::postgres(pool.clone(), cipher),
        channel_store: ChannelStore::postgres(pool),
        hermes_proxy: ReqwestHermesProxyClient::default().shared(),
        model_registry,
        llm_provider: ReqwestLlmProviderClient::default().shared(),
        docker_provisioner,
    };

    Ok(build_router_with_state(state))
}

pub fn build_router_with_state(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();
    let index_file = static_dir.join("index.html");
    // 后端作为 Web 服务器托管前端构建产物；SPA 深链统一回落到 index.html。
    let static_assets = ServeDir::new(static_dir).fallback(ServeFile::new(index_file));

    Router::new()
        .route("/health", get(health))
        .merge(http::router())
        .fallback_service(static_assets)
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub fn docker_config_from_app(
    config: &AppConfig,
    default_model: String,
) -> DockerProvisionerConfig {
    DockerProvisionerConfig {
        image: config.hermes_docker.image.clone(),
        data_root: config.hermes_docker.data_root.clone(),
        network: config.hermes_docker.network.clone(),
        internal_port: config.hermes_docker.internal_port,
        connect_mode: config.hermes_docker.connect_mode.clone(),
        published_host_ip: config.hermes_docker.published_host_ip.clone(),
        published_base_url: config.hermes_docker.published_base_url.clone(),
        hub_llm_base_url: config.hermes_docker.hub_llm_base_url.clone(),
        default_model,
        memory_limit: config.hermes_docker.memory_limit.clone(),
        cpu_limit: config.hermes_docker.cpu_limit.clone(),
        docker_binary: config.hermes_docker.docker_binary.clone(),
    }
}
