use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use crate::hermes::docker_provisioner::HermesContainerConnectMode;
use crate::model_config::{
    normalize_reasoning_effort, ModelConfig, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND,
};

/// 应用启动配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub cookie_name: String,
    pub database_url: Option<String>,
    pub secret_master_key: Option<String>,
    pub initial_model_config: ModelConfig,
    pub hermes_docker: HermesDockerConfig,
    pub object_storage: ObjectStorageConfig,
    pub max_proxy_body_bytes: usize,
    pub static_dir: PathBuf,
}

/// Hub 托管 Hermes 容器时使用的 Docker 配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HermesDockerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub connect_mode: HermesContainerConnectMode,
    pub published_host_ip: String,
    pub published_base_url: String,
    pub hub_llm_base_url: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
}

/// Hub 文件服务使用的 S3-compatible 对象存储配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectStorageConfig {
    pub endpoint: Option<String>,
    pub bucket: String,
    pub region: String,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub force_path_style: bool,
    pub prefix: String,
    pub max_upload_bytes: usize,
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
            object_storage: default_object_storage_config(),
            max_proxy_body_bytes: 10 * 1024 * 1024,
            static_dir: PathBuf::from("frontend/dist"),
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
            object_storage: object_storage_config_from_env(),
            max_proxy_body_bytes: env_usize("HERMES_HUB_MAX_PROXY_BODY_BYTES", 10 * 1024 * 1024),
            static_dir: PathBuf::from(
                std::env::var("HERMES_HUB_STATIC_DIR")
                    .unwrap_or_else(|_| "frontend/dist".to_string()),
            ),
        }
    }
}

fn object_storage_config_from_env() -> ObjectStorageConfig {
    ObjectStorageConfig {
        endpoint: optional_env_any(&[
            "HERMES_OBJECT_STORAGE_ENDPOINT",
            "HERMES_HUB_OBJECT_STORAGE_ENDPOINT",
        ]),
        bucket: env_any(&[
            "HERMES_OBJECT_STORAGE_BUCKET",
            "HERMES_HUB_OBJECT_STORAGE_BUCKET",
        ])
        .unwrap_or_else(|_| "hermes-hub".to_string()),
        region: env_any(&[
            "HERMES_OBJECT_STORAGE_REGION",
            "HERMES_HUB_OBJECT_STORAGE_REGION",
        ])
        .unwrap_or_else(|_| "us-east-1".to_string()),
        access_key: optional_env_any(&[
            "HERMES_OBJECT_STORAGE_ACCESS_KEY",
            "HERMES_HUB_OBJECT_STORAGE_ACCESS_KEY",
        ]),
        secret_key: optional_env_any(&[
            "HERMES_OBJECT_STORAGE_SECRET_KEY",
            "HERMES_HUB_OBJECT_STORAGE_SECRET_KEY",
        ]),
        force_path_style: env_any(&[
            "HERMES_OBJECT_STORAGE_FORCE_PATH_STYLE",
            "HERMES_HUB_OBJECT_STORAGE_FORCE_PATH_STYLE",
        ])
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true),
        prefix: env_any(&[
            "HERMES_OBJECT_STORAGE_PREFIX",
            "HERMES_HUB_OBJECT_STORAGE_PREFIX",
        ])
        .unwrap_or_else(|_| "attachments".to_string()),
        max_upload_bytes: env_usize_any(
            &[
                "HERMES_OBJECT_STORAGE_MAX_UPLOAD_BYTES",
                "HERMES_HUB_OBJECT_STORAGE_MAX_UPLOAD_BYTES",
            ],
            25 * 1024 * 1024,
        ),
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
        .unwrap_or(300);
    let allow_streaming = std::env::var("HERMES_HUB_MODEL_ALLOW_STREAMING")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true);
    let api_type = std::env::var("HERMES_HUB_MODEL_API_TYPE")
        .unwrap_or_else(|_| CHAT_COMPLETIONS_API_TYPE.to_string());
    let reasoning_effort =
        normalize_reasoning_effort(std::env::var("HERMES_HUB_MODEL_REASONING_EFFORT").ok())
            .unwrap_or(None);

    ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: std::env::var("HERMES_HUB_MODEL_PROVIDER_NAME")
            .unwrap_or_else(|_| "openai-compatible".to_string()),
        provider_base_url: std::env::var("HERMES_HUB_MODEL_PROVIDER_BASE_URL")
            .unwrap_or_else(|_| "https://provider.example/v1".to_string()),
        provider_api_key: std::env::var("HERMES_HUB_MODEL_PROVIDER_API_KEY")
            .unwrap_or_else(|_| "provider-secret".to_string()),
        default_model,
        allowed_models,
        api_type,
        reasoning_effort,
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
        connect_mode: std::env::var("HERMES_CONTAINER_CONNECT_MODE")
            .ok()
            .map(|value| HermesContainerConnectMode::parse(&value))
            .unwrap_or(HermesContainerConnectMode::Network),
        published_host_ip: std::env::var("HERMES_CONTAINER_PUBLISHED_HOST_IP")
            .unwrap_or_else(|_| "127.0.0.1".to_string()),
        published_base_url: std::env::var("HERMES_CONTAINER_PUBLISHED_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1".to_string()),
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
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
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
        connect_mode: HermesContainerConnectMode::Network,
        published_host_ip: "127.0.0.1".to_string(),
        published_base_url: "http://127.0.0.1".to_string(),
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
        docker_binary: "docker".to_string(),
    }
}

fn default_object_storage_config() -> ObjectStorageConfig {
    ObjectStorageConfig {
        endpoint: None,
        bucket: "hermes-hub-test".to_string(),
        region: "us-east-1".to_string(),
        access_key: None,
        secret_key: None,
        force_path_style: true,
        prefix: "attachments".to_string(),
        max_upload_bytes: 25 * 1024 * 1024,
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn optional_env_any(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| optional_env(name))
}

fn env_any(names: &[&str]) -> Result<String, std::env::VarError> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .ok_or(std::env::VarError::NotPresent)
}

fn env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_usize_any(names: &[&str], default: usize) -> usize {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{model_config_from_env, object_storage_config_from_env};

    #[test]
    fn object_storage_accepts_hub_prefixed_env_aliases() {
        const NAMES: &[&str] = &[
            "HERMES_OBJECT_STORAGE_ENDPOINT",
            "HERMES_OBJECT_STORAGE_BUCKET",
            "HERMES_OBJECT_STORAGE_REGION",
            "HERMES_OBJECT_STORAGE_ACCESS_KEY",
            "HERMES_OBJECT_STORAGE_SECRET_KEY",
            "HERMES_OBJECT_STORAGE_FORCE_PATH_STYLE",
            "HERMES_OBJECT_STORAGE_PREFIX",
            "HERMES_OBJECT_STORAGE_MAX_UPLOAD_BYTES",
            "HERMES_HUB_OBJECT_STORAGE_ENDPOINT",
            "HERMES_HUB_OBJECT_STORAGE_BUCKET",
            "HERMES_HUB_OBJECT_STORAGE_REGION",
            "HERMES_HUB_OBJECT_STORAGE_ACCESS_KEY",
            "HERMES_HUB_OBJECT_STORAGE_SECRET_KEY",
            "HERMES_HUB_OBJECT_STORAGE_FORCE_PATH_STYLE",
            "HERMES_HUB_OBJECT_STORAGE_PREFIX",
            "HERMES_HUB_OBJECT_STORAGE_MAX_UPLOAD_BYTES",
        ];

        let saved = NAMES
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect::<Vec<_>>();
        for name in NAMES {
            std::env::remove_var(name);
        }

        // 本地调试和旧部署命令可能使用 HERMES_HUB_OBJECT_STORAGE_*；
        // 后端需要兼容该前缀，避免附件存储静默退回内存实现。
        std::env::set_var(
            "HERMES_HUB_OBJECT_STORAGE_ENDPOINT",
            "http://127.0.0.1:9000",
        );
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_BUCKET", "hub-bucket");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_REGION", "local");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_ACCESS_KEY", "access");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_SECRET_KEY", "secret");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_FORCE_PATH_STYLE", "false");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_PREFIX", "files");
        std::env::set_var("HERMES_HUB_OBJECT_STORAGE_MAX_UPLOAD_BYTES", "123");

        let config = object_storage_config_from_env();
        assert_eq!(config.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(config.bucket, "hub-bucket");
        assert_eq!(config.region, "local");
        assert_eq!(config.access_key.as_deref(), Some("access"));
        assert_eq!(config.secret_key.as_deref(), Some("secret"));
        assert!(!config.force_path_style);
        assert_eq!(config.prefix, "files");
        assert_eq!(config.max_upload_bytes, 123);

        for (name, value) in saved {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }

    #[test]
    fn llm_model_config_uses_long_default_request_timeout() {
        let saved = std::env::var("HERMES_HUB_MODEL_REQUEST_TIMEOUT_SECONDS").ok();
        std::env::remove_var("HERMES_HUB_MODEL_REQUEST_TIMEOUT_SECONDS");

        // Hermes 的 agent 任务会长期持有流式模型连接；默认值不能按普通短请求的 60 秒处理。
        let config = model_config_from_env();
        assert_eq!(config.request_timeout_seconds, 300);

        if let Some(value) = saved {
            std::env::set_var("HERMES_HUB_MODEL_REQUEST_TIMEOUT_SECONDS", value);
        }
    }
}
