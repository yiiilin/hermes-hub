pub mod app_config;
pub mod channel;
pub mod db;
pub mod hermes;
pub mod http;
pub mod llm_proxy;
pub mod model_config;
pub mod model_registry;
pub mod security;
pub mod skills_fs;
pub mod storage;
pub mod session {
    pub mod store;
}
pub mod domain {
    pub mod invite;
    pub mod user;
}

use axum::{
    extract::DefaultBodyLimit,
    routing::{any, get},
    Json, Router,
};
use channel::service::ChannelStore;
use hermes::docker_provisioner::{
    DockerProvisioner, DockerProvisionerConfig, ManagedSkillsMountConfig, NoopDockerRuntime,
};
use llm_proxy::{DynLlmProviderClient, InMemoryLlmProviderClient, ReqwestLlmProviderClient};
use model_config::{ModelConfig, ModelRegistry};
use serde::Serialize;
use session::store::SessionStore;
use std::sync::Arc;
use storage::{object_storage_from_config, DynObjectStorage};
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
    pub model_registry: ModelRegistry,
    pub llm_provider: DynLlmProviderClient,
    pub docker_provisioner: DockerProvisioner,
    pub object_storage: DynObjectStorage,
    pub session_events: channel::events::SessionEventHub,
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
        docker_config_from_app(&config, &config.initial_model_config),
        Arc::new(NoopDockerRuntime),
    );
    let object_storage = object_storage_from_config(&config.object_storage);
    let state = AppState {
        model_registry: ModelRegistry::new(config.initial_model_config.clone()),
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: channel::events::SessionEventHub::default(),
    };

    build_router_with_state(state)
}

/// 根据运行时配置构建 Router；存在 DATABASE_URL 时启用 PostgreSQL 后端。
pub async fn build_router_from_config(config: AppConfig) -> Result<Router, AppInitError> {
    let docker_provisioner = DockerProvisioner::new(docker_config_from_app(
        &config,
        &config.initial_model_config,
    ));
    let object_storage = object_storage_from_config(&config.object_storage);

    let Some(database_url) = config.database_url.clone() else {
        let state = AppState {
            model_registry: ModelRegistry::new(config.initial_model_config.clone()),
            config,
            store: SessionStore::default(),
            channel_store: ChannelStore::default(),
            llm_provider: ReqwestLlmProviderClient::default().shared(),
            docker_provisioner,
            object_storage,
            session_events: channel::events::SessionEventHub::default(),
        };
        tokio::spawn(hermes::lifecycle::start_hermes_lifecycle_sweeper(
            state.clone(),
        ));
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
        model_registry,
        llm_provider: ReqwestLlmProviderClient::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: channel::events::SessionEventHub::default(),
    };
    tokio::spawn(hermes::lifecycle::start_hermes_lifecycle_sweeper(
        state.clone(),
    ));

    Ok(build_router_with_state(state))
}

pub fn build_router_with_state(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();
    let index_file = static_dir.join("index.html");
    let request_body_limit = state
        .config
        .object_storage
        .max_upload_bytes
        .saturating_add(1024 * 1024);
    // 后端作为 Web 服务器托管前端构建产物；SPA 深链统一回落到 index.html。
    let static_assets = ServeDir::new(static_dir).fallback(ServeFile::new(index_file));

    Router::new()
        .route("/health", get(health))
        .merge(http::router())
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(static_assets)
        // Axum multipart 默认只允许约 2MB；Hub 的真实文件上限由对象存储配置控制。
        .layer(DefaultBodyLimit::max(request_body_limit))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn api_not_found() -> http::ApiError {
    // API 未匹配时不能继续落到 SPA 静态文件 fallback，否则 POST 会变成含糊的 405。
    http::ApiError::NotFound("api route not found")
}

pub fn docker_config_from_app(
    config: &AppConfig,
    model_config: &ModelConfig,
) -> DockerProvisionerConfig {
    DockerProvisionerConfig {
        image: config.hermes_docker.image.clone(),
        data_root: config.hermes_docker.data_root.clone(),
        network: config.hermes_docker.network.clone(),
        internal_port: config.hermes_docker.internal_port,
        hub_llm_base_url: config.hermes_docker.hub_llm_base_url.clone(),
        default_model: model_config.default_model.clone(),
        context_window_tokens: model_config.context_window_tokens,
        max_output_tokens: model_config.max_output_tokens,
        temperature: model_config.temperature,
        supports_parallel_tools: model_config.supports_parallel_tools,
        // 启动时还没有读取管理员的图片模型配置；实际创建/重建容器时会用数据库中的
        // image 配置覆盖这里的默认值。
        image_model_enabled: false,
        image_model: "gpt-image-1".to_string(),
        api_mode: model_config.api_type.clone(),
        memory_limit: config.hermes_docker.memory_limit.clone(),
        cpu_limit: config.hermes_docker.cpu_limit.clone(),
        docker_binary: config.hermes_docker.docker_binary.clone(),
        managed_skills: config
            .skills_fs
            .mount_enabled
            .then(|| ManagedSkillsMountConfig {
                volume_name: config.skills_fs.mount_volume_name.clone(),
                addr: config.skills_fs.mount_addr.clone(),
                export: config.skills_fs.mount_export.clone(),
                container_path: config.skills_fs.container_path.clone(),
            }),
    }
}
