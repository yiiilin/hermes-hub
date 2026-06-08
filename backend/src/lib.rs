#![recursion_limit = "256"]

pub mod app_config;
pub mod asr;
pub mod channel;
pub mod db;
pub mod hermes;
pub mod http;
pub mod ldap;
pub mod llm_proxy;
pub mod model_config;
pub mod model_registry;
pub mod public_platform;
pub mod security;
pub mod skills_fs;
pub mod storage;
pub mod title_generation;
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
    DockerBackendRuntimeHint, DockerProvisioner, DockerProvisionerConfig, ManagedProfileConfig,
    ManagedSkillsMountConfig,
};
use ldap::{DefaultLdapAuthenticator, DynLdapAuthenticator};
use llm_proxy::{DynLlmProviderClient, ReqwestLlmProviderClient};
use model_config::{ModelConfig, ModelRegistry};
use serde::Serialize;
use session::store::SessionStore;
use std::path::Path;
use std::sync::Arc;
use storage::{object_storage_from_config, DynObjectStorage, ObjectStorageError};
use thiserror::Error;
use tower_http::services::{ServeDir, ServeFile};

pub use app_config::AppConfig;

const DEFAULT_REQUEST_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

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
    pub ldap_authenticator: DynLdapAuthenticator,
    pub docker_provisioner: DockerProvisioner,
    pub object_storage: DynObjectStorage,
    pub session_events: channel::events::SessionEventHub,
}

#[derive(Debug, Error)]
pub enum AppInitError {
    #[error("database url is required for runtime startup")]
    MissingDatabaseUrl,
    #[error("HERMES_DATA_ROOT must be an absolute host path")]
    InvalidHermesDataRoot,
    #[error("database connection failed")]
    Database(#[from] sqlx::Error),
    #[error("secret master key is required for runtime startup")]
    MissingSecretMasterKey,
    #[error("secret master key is invalid")]
    InvalidSecretMasterKey(#[from] security::crypto::SecretCipherError),
    #[error("model registry initialization failed")]
    ModelRegistry(#[from] model_config::ModelRegistryError),
    #[error("object storage initialization failed")]
    ObjectStorage(#[from] ObjectStorageError),
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// 根据运行时配置构建 Router。
///
/// 真实运行时强制要求 PostgreSQL 持久化；未配置数据库时直接启动失败，
/// 避免默默退回 memory store，导致线上与持久化模式语义不一致。
pub async fn build_router_from_config(config: AppConfig) -> Result<Router, AppInitError> {
    let mut config = config;
    normalize_runtime_paths(&mut config)?;
    let database_url = config
        .database_url
        .clone()
        .ok_or(AppInitError::MissingDatabaseUrl)?;
    let secret_master_key = config
        .secret_master_key
        .clone()
        .ok_or(AppInitError::MissingSecretMasterKey)?;
    let object_storage = object_storage_from_config(&config.object_storage)?;
    let docker_provisioner = DockerProvisioner::new_with_runtime_and_object_storage(
        docker_config_from_app(&config, &config.initial_model_config),
        Arc::new(hermes::docker_provisioner::CommandDockerRuntime::new(
            config.hermes_docker.docker_binary.clone(),
        )),
        object_storage.clone(),
    );
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
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: channel::events::SessionEventHub::default(),
    };
    tokio::spawn(hermes::lifecycle::start_hermes_lifecycle_sweeper(
        state.clone(),
    ));

    Ok(build_router_with_state(state))
}

fn normalize_runtime_paths(config: &mut AppConfig) -> Result<(), AppInitError> {
    ensure_absolute_path(&config.hermes_docker.data_root)?;
    Ok(())
}

fn ensure_absolute_path(path: &Path) -> Result<(), AppInitError> {
    if !path.is_absolute() {
        return Err(AppInitError::InvalidHermesDataRoot);
    }
    Ok(())
}

pub fn build_router_with_state(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();
    let index_file = static_dir.join("index.html");
    let examples_dir = static_dir.join("examples");
    // 后端作为 Web 服务器托管前端构建产物；SPA 深链统一回落到 index.html。
    let static_assets = ServeDir::new(static_dir).fallback(ServeFile::new(index_file));
    // /examples 只按真实静态文件提供；正式包没打进来时必须明确 404，不能误落到主应用 SPA。
    let example_assets = ServeDir::new(examples_dir);

    Router::new()
        .route("/health", get(health))
        .merge(http::router())
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .nest_service("/examples", example_assets)
        .fallback_service(static_assets)
        // 普通 JSON/API 请求保持小 body limit；大附件上传路由单独禁用框架限制并流式校验。
        .layer(DefaultBodyLimit::max(DEFAULT_REQUEST_BODY_LIMIT_BYTES))
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
        backend_runtime_hint: DockerBackendRuntimeHint::Auto,
        network_gateway_hint: None,
        managed_skills: config
            .skills_fs
            .mount_enabled
            .then(|| ManagedSkillsMountConfig {
                volume_name: config.skills_fs.mount_volume_name.clone(),
                addr: config.skills_fs.mount_addr.clone(),
                export: config.skills_fs.mount_export.clone(),
                container_path: config.skills_fs.container_path.clone(),
            }),
        managed_profile: (config.managed_profile.enabled && config.skills_fs.mount_enabled).then(
            || ManagedProfileConfig {
                // 统一 SOUL.md 通过同一个 Hub FS 挂载进入容器；wrapper entrypoint
                // 会从该目录把文件链接到 Hermes 会读取的位置。
                container_path: config.skills_fs.container_path.clone(),
                object_prefix: config.managed_profile.prefix.clone(),
            },
        ),
    }
}
