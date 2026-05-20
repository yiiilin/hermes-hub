use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use crate::model_config::ModelConfig;

/// 应用启动配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub cookie_name: String,
    pub database_url: Option<String>,
    pub secret_master_key: Option<String>,
    pub initial_model_config: ModelConfig,
    pub hermes_docker: HermesDockerConfig,
    pub proxy_timeout_seconds: u64,
    pub max_proxy_body_bytes: usize,
}

/// Hub 托管 Hermes 容器时使用的 Docker 配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HermesDockerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub hub_llm_base_url: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
}

impl AppConfig {
    /// 测试环境使用固定的本地配置，避免依赖真实端口和外部环境变量。
    pub fn for_tests() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            cookie_name: "hermes_hub_session".to_string(),
            database_url: None,
            secret_master_key: None,
            initial_model_config: default_model_config(),
            hermes_docker: default_hermes_docker_config(),
            proxy_timeout_seconds: 60,
            max_proxy_body_bytes: 10 * 1024 * 1024,
        }
    }

    /// 运行时配置从环境变量读取，未配置时使用可在本地启动的默认值。
    pub fn from_env() -> Self {
        let bind_addr = std::env::var("HERMES_HUB_BIND_ADDR")
            .ok()
            .and_then(|value| SocketAddr::from_str(&value).ok())
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080));

        Self {
            bind_addr,
            cookie_name: std::env::var("HERMES_HUB_COOKIE_NAME")
                .unwrap_or_else(|_| "hermes_hub_session".to_string()),
            database_url: std::env::var("DATABASE_URL").ok(),
            secret_master_key: std::env::var("HERMES_HUB_SECRET_MASTER_KEY").ok(),
            initial_model_config: model_config_from_env(),
            hermes_docker: hermes_docker_config_from_env(),
            proxy_timeout_seconds: env_u64("HERMES_HUB_PROXY_TIMEOUT_SECONDS", 60),
            max_proxy_body_bytes: env_usize(
                "HERMES_HUB_MAX_PROXY_BODY_BYTES",
                10 * 1024 * 1024,
            ),
        }
    }
}

fn model_config_from_env() -> ModelConfig {
    let default_model =
        std::env::var("HERMES_HUB_DEFAULT_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
    let allowed_models = std::env::var("HERMES_HUB_ALLOWED_MODELS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| vec![default_model.clone()]);
    let request_timeout_seconds = std::env::var("HERMES_HUB_MODEL_REQUEST_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    let allow_streaming = std::env::var("HERMES_HUB_MODEL_ALLOW_STREAMING")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true);

    ModelConfig {
        provider_name: std::env::var("HERMES_HUB_MODEL_PROVIDER_NAME")
            .unwrap_or_else(|_| "openai-compatible".to_string()),
        provider_base_url: std::env::var("HERMES_HUB_MODEL_PROVIDER_BASE_URL")
            .unwrap_or_else(|_| "https://provider.example/v1".to_string()),
        provider_api_key: std::env::var("HERMES_HUB_MODEL_PROVIDER_API_KEY")
            .unwrap_or_else(|_| "provider-secret".to_string()),
        default_model,
        allowed_models,
        allow_streaming,
        request_timeout_seconds,
    }
}

fn hermes_docker_config_from_env() -> HermesDockerConfig {
    HermesDockerConfig {
        image: std::env::var("HERMES_DOCKER_IMAGE")
            .unwrap_or_else(|_| "nousresearch/hermes-agent:latest".to_string()),
        data_root: PathBuf::from(
            std::env::var("HERMES_DATA_ROOT")
                .unwrap_or_else(|_| "/data/hermes-hub/users".to_string()),
        ),
        network: std::env::var("HERMES_CONTAINER_NETWORK")
            .unwrap_or_else(|_| "hermes-hub-net".to_string()),
        internal_port: env_u16("HERMES_CONTAINER_INTERNAL_PORT", 8000),
        hub_llm_base_url: std::env::var("HERMES_HUB_LLM_BASE_URL")
            .unwrap_or_else(|_| "http://hermes-hub:8080/internal/llm/v1".to_string()),
        memory_limit: optional_env("HERMES_CONTAINER_MEMORY_LIMIT").or(Some("1g".to_string())),
        cpu_limit: optional_env("HERMES_CONTAINER_CPU_LIMIT").or(Some("1.0".to_string())),
        docker_binary: std::env::var("HERMES_DOCKER_BINARY")
            .unwrap_or_else(|_| "docker".to_string()),
    }
}

fn default_model_config() -> ModelConfig {
    ModelConfig {
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        allow_streaming: true,
        request_timeout_seconds: 60,
    }
}

fn default_hermes_docker_config() -> HermesDockerConfig {
    HermesDockerConfig {
        image: "nousresearch/hermes-agent:latest".to_string(),
        data_root: PathBuf::from("/tmp/hermes-hub/users"),
        network: "hermes-hub-net".to_string(),
        internal_port: 8000,
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
        docker_binary: "docker".to_string(),
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}
