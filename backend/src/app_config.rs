use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

use crate::model_config::{
    normalize_reasoning_effort, ModelConfig, CHAT_COMPLETIONS_API_TYPE,
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_TEMPERATURE,
    LLM_MODEL_CONFIG_KIND,
};

/// 应用启动配置。
#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub cookie_name: String,
    pub database_url: Option<String>,
    pub secret_master_key: Option<String>,
    pub initial_model_config: ModelConfig,
    pub hermes_docker: HermesDockerConfig,
    pub object_storage: ObjectStorageConfig,
    pub skills_fs: SkillsFsConfig,
    pub managed_profile: ManagedProfileConfig,
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
}

/// 统一 skill 文件系统服务配置。服务进程用它启动只读 NFS；backend 用其中的
/// Docker volume 配置把该 NFS export 挂进每个托管 Hermes 容器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillsFsConfig {
    pub bind_addr: SocketAddr,
    pub prefix: String,
    pub export_name: String,
    pub mount_enabled: bool,
    pub mount_volume_name: String,
    pub mount_addr: String,
    pub mount_export: String,
    pub container_path: String,
}

/// 统一 Hermes SOUL.md 配置。
/// 文件存储在对象存储中，并由同一个 hermes-hub-fs NFS 导出给 Hermes 容器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedProfileConfig {
    pub enabled: bool,
    pub prefix: String,
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
            skills_fs: default_skills_fs_config(),
            managed_profile: default_managed_profile_config(),
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
            skills_fs: skills_fs_config_from_env(),
            managed_profile: managed_profile_config_from_env(),
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
    let context_window_tokens = std::env::var("HERMES_HUB_MODEL_CONTEXT_WINDOW_TOKENS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
    let max_output_tokens = std::env::var("HERMES_HUB_MODEL_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let temperature = std::env::var("HERMES_HUB_MODEL_TEMPERATURE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| (0.0..=2.0).contains(value))
        .unwrap_or(DEFAULT_TEMPERATURE);
    let supports_parallel_tools = std::env::var("HERMES_HUB_MODEL_SUPPORTS_PARALLEL_TOOLS")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true);

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
        enabled: true,
        allow_streaming,
        request_timeout_seconds,
        context_window_tokens,
        max_output_tokens,
        temperature,
        supports_parallel_tools,
        fallback: None,
    }
}

fn hermes_docker_config_from_env() -> HermesDockerConfig {
    HermesDockerConfig {
        image: std::env::var("HERMES_DOCKER_IMAGE")
            .unwrap_or_else(|_| "ghcr.io/yiiilin/hermes-hub-hermes:latest".to_string()),
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

fn skills_fs_config_from_env() -> SkillsFsConfig {
    SkillsFsConfig {
        bind_addr: std::env::var("HERMES_HUB_SKILLS_FS_BIND_ADDR")
            .ok()
            .and_then(|value| SocketAddr::from_str(&value).ok())
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 12049)),
        prefix: std::env::var("HERMES_HUB_SKILLS_FS_PREFIX")
            .unwrap_or_else(|_| "managed-skills/current".to_string()),
        export_name: std::env::var("HERMES_HUB_SKILLS_FS_EXPORT_NAME")
            .unwrap_or_else(|_| "skills".to_string()),
        mount_enabled: std::env::var("HERMES_HUB_MANAGED_SKILLS_MOUNT_ENABLED")
            .ok()
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(true),
        mount_volume_name: std::env::var("HERMES_HUB_MANAGED_SKILLS_VOLUME_NAME")
            .unwrap_or_else(|_| "hermes-hub-managed-skills".to_string()),
        mount_addr: std::env::var("HERMES_HUB_MANAGED_SKILLS_NFS_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:12049".to_string()),
        mount_export: std::env::var("HERMES_HUB_MANAGED_SKILLS_NFS_EXPORT")
            .unwrap_or_else(|_| "/skills".to_string()),
        // Hermes 配置固定写入 /nfs/skills；容器挂载点也必须固定，避免旧环境变量把挂载漂回旧路径。
        container_path: "/nfs".to_string(),
    }
}

fn managed_profile_config_from_env() -> ManagedProfileConfig {
    ManagedProfileConfig {
        enabled: std::env::var("HERMES_HUB_MANAGED_PROFILE_ENABLED")
            .ok()
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(true),
        prefix: std::env::var("HERMES_HUB_MANAGED_PROFILE_PREFIX")
            .or_else(|_| std::env::var("HERMES_HUB_PROFILE_FS_PREFIX"))
            .unwrap_or_else(|_| "managed-profile/current".to_string()),
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
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        temperature: DEFAULT_TEMPERATURE,
        supports_parallel_tools: true,
        fallback: None,
    }
}

fn default_hermes_docker_config() -> HermesDockerConfig {
    HermesDockerConfig {
        image: "ghcr.io/yiiilin/hermes-hub-hermes:latest".to_string(),
        data_root: PathBuf::from("/tmp/hermes-hub/users"),
        network: "hermes-hub-net".to_string(),
        internal_port: 8000,
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
    }
}

fn default_skills_fs_config() -> SkillsFsConfig {
    SkillsFsConfig {
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 12049),
        prefix: "managed-skills/current".to_string(),
        export_name: "skills".to_string(),
        mount_enabled: false,
        mount_volume_name: "hermes-hub-managed-skills-test".to_string(),
        mount_addr: "127.0.0.1:12049".to_string(),
        mount_export: "/skills".to_string(),
        container_path: "/nfs".to_string(),
    }
}

fn default_managed_profile_config() -> ManagedProfileConfig {
    ManagedProfileConfig {
        enabled: true,
        prefix: "managed-profile/current".to_string(),
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

#[cfg(test)]
mod tests {
    use super::{
        hermes_docker_config_from_env, managed_profile_config_from_env, model_config_from_env,
        object_storage_config_from_env, skills_fs_config_from_env,
    };

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
            "HERMES_HUB_OBJECT_STORAGE_ENDPOINT",
            "HERMES_HUB_OBJECT_STORAGE_BUCKET",
            "HERMES_HUB_OBJECT_STORAGE_REGION",
            "HERMES_HUB_OBJECT_STORAGE_ACCESS_KEY",
            "HERMES_HUB_OBJECT_STORAGE_SECRET_KEY",
            "HERMES_HUB_OBJECT_STORAGE_FORCE_PATH_STYLE",
            "HERMES_HUB_OBJECT_STORAGE_PREFIX",
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

        let config = object_storage_config_from_env();
        assert_eq!(config.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(config.bucket, "hub-bucket");
        assert_eq!(config.region, "local");
        assert_eq!(config.access_key.as_deref(), Some("access"));
        assert_eq!(config.secret_key.as_deref(), Some("secret"));
        assert!(!config.force_path_style);
        assert_eq!(config.prefix, "files");

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

    #[test]
    fn hermes_docker_config_defaults_to_ghcr_wrapper_image() {
        let saved = std::env::var("HERMES_DOCKER_IMAGE").ok();
        std::env::remove_var("HERMES_DOCKER_IMAGE");

        // Hub 托管的 Hermes 运行时默认使用 GHCR 上的薄包装镜像；
        // 具体官方 Hermes 版本由镜像 tag 跟随。
        let config = hermes_docker_config_from_env();
        assert_eq!(config.image, "ghcr.io/yiiilin/hermes-hub-hermes:latest");

        if let Some(value) = saved {
            std::env::set_var("HERMES_DOCKER_IMAGE", value);
        }
    }

    #[test]
    fn skills_fs_config_reads_nfs_env_and_pins_container_mount_path() {
        const NAMES: &[&str] = &[
            "HERMES_HUB_SKILLS_FS_BIND_ADDR",
            "HERMES_HUB_SKILLS_FS_PREFIX",
            "HERMES_HUB_SKILLS_FS_EXPORT_NAME",
            "HERMES_HUB_MANAGED_SKILLS_MOUNT_ENABLED",
            "HERMES_HUB_MANAGED_SKILLS_VOLUME_NAME",
            "HERMES_HUB_MANAGED_SKILLS_NFS_ADDR",
            "HERMES_HUB_MANAGED_SKILLS_NFS_EXPORT",
            "HERMES_HUB_MANAGED_SKILLS_CONTAINER_PATH",
        ];
        let saved = NAMES
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect::<Vec<_>>();
        for name in NAMES {
            std::env::remove_var(name);
        }

        std::env::set_var("HERMES_HUB_SKILLS_FS_BIND_ADDR", "127.0.0.1:12050");
        std::env::set_var("HERMES_HUB_SKILLS_FS_PREFIX", "managed-skills/release-a");
        std::env::set_var("HERMES_HUB_SKILLS_FS_EXPORT_NAME", "hub-skills");
        std::env::set_var("HERMES_HUB_MANAGED_SKILLS_MOUNT_ENABLED", "false");
        std::env::set_var("HERMES_HUB_MANAGED_SKILLS_VOLUME_NAME", "skills-vol");
        std::env::set_var("HERMES_HUB_MANAGED_SKILLS_NFS_ADDR", "10.0.0.5:12049");
        std::env::set_var("HERMES_HUB_MANAGED_SKILLS_NFS_EXPORT", "/hub-skills");
        std::env::set_var(
            "HERMES_HUB_MANAGED_SKILLS_CONTAINER_PATH",
            "/managed-skills",
        );

        let config = skills_fs_config_from_env();
        assert_eq!(config.bind_addr.to_string(), "127.0.0.1:12050");
        assert_eq!(config.prefix, "managed-skills/release-a");
        assert_eq!(config.export_name, "hub-skills");
        assert!(!config.mount_enabled);
        assert_eq!(config.mount_volume_name, "skills-vol");
        assert_eq!(config.mount_addr, "10.0.0.5:12049");
        assert_eq!(config.mount_export, "/hub-skills");
        assert_eq!(config.container_path, "/nfs");

        for (name, value) in saved {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }

    #[test]
    fn managed_profile_config_reads_env() {
        const NAMES: &[&str] = &[
            "HERMES_HUB_MANAGED_PROFILE_ENABLED",
            "HERMES_HUB_MANAGED_PROFILE_PREFIX",
            "HERMES_HUB_PROFILE_FS_PREFIX",
        ];
        let saved = NAMES
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect::<Vec<_>>();
        for name in NAMES {
            std::env::remove_var(name);
        }

        std::env::set_var("HERMES_HUB_MANAGED_PROFILE_ENABLED", "false");
        std::env::set_var(
            "HERMES_HUB_MANAGED_PROFILE_PREFIX",
            "managed-profile/release-a",
        );

        let config = managed_profile_config_from_env();
        assert!(!config.enabled);
        assert_eq!(config.prefix, "managed-profile/release-a");

        std::env::remove_var("HERMES_HUB_MANAGED_PROFILE_PREFIX");
        std::env::set_var(
            "HERMES_HUB_PROFILE_FS_PREFIX",
            "managed-profile/legacy-name",
        );
        let config = managed_profile_config_from_env();
        assert_eq!(config.prefix, "managed-profile/legacy-name");

        for (name, value) in saved {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}
