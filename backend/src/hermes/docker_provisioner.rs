#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::process::Command;
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use crate::{
    model_config::RESPONSES_API_TYPE,
    storage::{DynObjectStorage, ObjectStorageError},
};

use super::{
    instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    provisioner::{HermesProvisioner, ProvisionerError},
};

/// Hub 托管 Hermes 容器规格版本。只要 env、挂载、工作目录或安全策略有变化，
/// 就提升这个值，确保已存在的旧容器会被重建并拿到新行为。
const MANAGED_CONTAINER_SPEC_VERSION: &str = "2026-06-03-home-channel";
const MANAGED_CONTAINER_SPEC_LABEL: &str = "hermes_hub_spec_version";
const HUB_INBOX_PATH: &str = "/internal/channel/v1/inbox";
const HUB_INBOX_TIMEOUT_SECONDS: u16 = 25;
const HUB_INBOX_LIMIT: u16 = 4;
const HUB_NFS_CONTAINER_DIR: &str = "/nfs";
const MANAGED_SKILLS_EXTERNAL_DIR: &str = "/nfs/skills";
const MANAGED_PROFILE_SOUL_FILE: &str = "SOUL.md";
// Hub 的附件语义是“Hermes 容器内可读文件都可以作为附件发回 Hub”；
// 容器本身是安全边界，因此这里显式把官方 MEDIA allow dirs 放宽到根目录。
const HERMES_MEDIA_ALLOW_DIRS: &str = "/";
const DOCKER_DAEMON_SOCKET: &str = "/var/run/docker.sock";
/// Docker 托管 Hermes 的运行配置。
#[derive(Clone, Debug, PartialEq)]
pub struct DockerProvisionerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub hub_llm_base_url: String,
    pub default_model: String,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
    pub image_model_enabled: bool,
    pub image_model: String,
    pub api_mode: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
    pub managed_skills: Option<ManagedSkillsMountConfig>,
    pub managed_profile: Option<ManagedProfileConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeModelSettings {
    pub default_model: String,
    pub api_mode: String,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
}

/// 容器挂载定义。测试和真实 Docker adapter 共用同一份 spec，避免部署行为漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ContainerMount {
    Bind(ContainerBindMount),
    NfsVolume(ContainerNfsVolumeMount),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerBindMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerNfsVolumeMount {
    pub volume_name: String,
    pub container_path: String,
    pub read_only: bool,
    pub addr: String,
    pub export: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSkillsMountConfig {
    pub volume_name: String,
    pub addr: String,
    pub export: String,
    pub container_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedProfileConfig {
    pub container_path: String,
    pub object_prefix: String,
}

impl ContainerMount {
    pub fn bind(host_path: String, container_path: impl Into<String>, read_only: bool) -> Self {
        Self::Bind(ContainerBindMount {
            host_path,
            container_path: container_path.into(),
            read_only,
        })
    }

    pub fn nfs_volume(config: &ManagedSkillsMountConfig, read_only: bool) -> Self {
        Self::NfsVolume(ContainerNfsVolumeMount {
            volume_name: managed_nfs_volume_name(&config.volume_name, read_only),
            // Hermes config 固定引用 /nfs/skills，挂载点也固定到 /nfs，避免旧配置造成路径漂移。
            container_path: HUB_NFS_CONTAINER_DIR.to_string(),
            read_only,
            addr: config.addr.clone(),
            export: config.export.clone(),
        })
    }

    pub fn container_path(&self) -> &str {
        match self {
            Self::Bind(mount) => &mount.container_path,
            Self::NfsVolume(mount) => &mount.container_path,
        }
    }

    pub fn read_only(&self) -> bool {
        match self {
            Self::Bind(mount) => mount.read_only,
            Self::NfsVolume(mount) => mount.read_only,
        }
    }
}

/// 可渲染为 Docker create 参数的规范。adapter-only 托管 Hermes 不包含任何端口发布配置。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub internal_port: u16,
    pub env: Vec<String>,
    pub mounts: Vec<ContainerMount>,
    pub labels: Vec<(String, String)>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub workdir: Option<String>,
    pub healthcheck: Option<ContainerHealthcheck>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContainerInspection {
    id: String,
    running: bool,
    health_status: Option<String>,
    health_error: Option<String>,
    spec_version: Option<String>,
    image: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerHealthcheck {
    pub command: String,
    pub interval: String,
    pub timeout: String,
    pub retries: u8,
    pub start_period: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockerRuntimeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait DockerRuntime: Send + Sync {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError>;
}

pub type DynDockerRuntime = Arc<dyn DockerRuntime>;

/// 生产 Docker runtime。它通过 Docker CLI 与本机 Docker daemon 交互，
/// 这样第一版不需要引入更重的 Docker API 客户端，也方便运维复用现有 docker 权限。
#[derive(Clone)]
pub struct CommandDockerRuntime {
    docker_binary: String,
    docker_api_version: Arc<Mutex<Option<String>>>,
}

impl CommandDockerRuntime {
    pub fn new(docker_binary: String) -> Self {
        Self {
            docker_binary,
            docker_api_version: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn new_with_cached_docker_api_version(
        docker_binary: String,
        docker_api_version: Option<String>,
    ) -> Self {
        Self {
            docker_binary,
            docker_api_version: Arc::new(Mutex::new(docker_api_version)),
        }
    }
}

#[async_trait]
impl DockerRuntime for CommandDockerRuntime {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let explicit_api_version = std::env::var_os("DOCKER_API_VERSION");
        if docker_api_version_env_is_explicit(explicit_api_version.as_deref()) {
            return self.run_once(&args, None).await;
        }

        let cached_api_version = self
            .docker_api_version
            .lock()
            .map_err(|error| ProvisionerError::DockerRuntime(error.to_string()))?
            .clone();
        if let Some(api_version) = cached_api_version {
            return self.run_once(&args, Some(api_version.as_str())).await;
        }

        let output = self.run_once(&args, None).await?;
        if output.success || !docker_client_api_is_too_new(&output.stderr) {
            return Ok(output);
        }

        if let Some(api_version) = detect_docker_daemon_api_version().await {
            if let Ok(mut cached) = self.docker_api_version.lock() {
                *cached = Some(api_version.clone());
            }
            return self.run_once(&args, Some(api_version.as_str())).await;
        }

        Ok(output)
    }
}

impl CommandDockerRuntime {
    async fn run_once(
        &self,
        args: &[String],
        docker_api_version: Option<&str>,
    ) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let mut command = Command::new(&self.docker_binary);
        command.args(args);

        if let Some(api_version) = docker_api_version.filter(|value| !value.trim().is_empty()) {
            // 只有确认当前 CLI 对 daemon 来说太新时才降级 API，避免破坏旧 CLI 自己的协商逻辑。
            command.env("DOCKER_API_VERSION", api_version);
        }

        let output = command
            .output()
            .await
            .map_err(|error| ProvisionerError::DockerRuntime(error.to_string()))?;

        Ok(DockerRuntimeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

fn docker_api_version_env_is_explicit(process_env_value: Option<&OsStr>) -> bool {
    process_env_value
        .map(|value| !value.to_string_lossy().trim().is_empty())
        .unwrap_or(false)
}

fn docker_client_api_is_too_new(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("client version")
        && stderr.contains("too new")
        && stderr.contains("maximum supported api version")
}

#[cfg(unix)]
async fn detect_docker_daemon_api_version() -> Option<String> {
    let socket_path = docker_socket_path_from_env(std::env::var_os("DOCKER_HOST").as_deref())?;
    let timeout = Duration::from_millis(500);
    let mut stream = tokio::time::timeout(timeout, UnixStream::connect(socket_path))
        .await
        .ok()?
        .ok()?;

    tokio::time::timeout(
        timeout,
        stream.write_all(b"GET /version HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n"),
    )
    .await
    .ok()?
    .ok()?;

    let mut response = String::new();
    tokio::time::timeout(timeout, stream.read_to_string(&mut response))
        .await
        .ok()?
        .ok()?;

    parse_docker_api_version_response(&response)
}

#[cfg(not(unix))]
async fn detect_docker_daemon_api_version() -> Option<String> {
    None
}

#[cfg(unix)]
fn docker_socket_path_from_env(docker_host: Option<&OsStr>) -> Option<PathBuf> {
    let Some(docker_host) = docker_host else {
        return Some(PathBuf::from(DOCKER_DAEMON_SOCKET));
    };
    let docker_host = docker_host.to_string_lossy();
    let docker_host = docker_host.trim();

    if docker_host.is_empty() {
        return Some(PathBuf::from(DOCKER_DAEMON_SOCKET));
    }

    docker_host
        .strip_prefix("unix://")
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn parse_docker_api_version_response(response: &str) -> Option<String> {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(response)
        .trim();
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .get("ApiVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// 单元测试和内存演示模式使用的 Docker runtime。它不碰真实 Docker daemon，
/// 只返回稳定成功结果；真实启动路径不会使用它。
#[derive(Clone, Default)]
pub struct NoopDockerRuntime;

#[async_trait]
impl DockerRuntime for NoopDockerRuntime {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let stdout = if args.get(0).map(String::as_str) == Some("container")
            && args.get(1).map(String::as_str) == Some("inspect")
        {
            format!(
                r#"{{"Id":"noop-container-id","State":{{"Running":true}},"Config":{{"Labels":{{"{MANAGED_CONTAINER_SPEC_LABEL}":"{MANAGED_CONTAINER_SPEC_VERSION}"}}}}}}"#
            )
        } else if args.first().map(String::as_str) == Some("create") {
            "noop-container-id".to_string()
        } else {
            String::new()
        };

        Ok(DockerRuntimeOutput {
            success: true,
            stdout,
            stderr: String::new(),
        })
    }
}

/// Docker provisioner 会真实创建/启动/停止容器；内存 map 只用于测试和
/// handler 当前进程内快速读取最近一次编排结果，权威状态仍写入数据库。
#[derive(Clone)]
pub struct DockerProvisioner {
    config: DockerProvisionerConfig,
    runtime: DynDockerRuntime,
    object_storage: Option<DynObjectStorage>,
    instances: Arc<Mutex<HashMap<String, HermesInstance>>>,
}

impl DockerProvisioner {
    pub fn new(config: DockerProvisionerConfig) -> Self {
        let runtime = Arc::new(CommandDockerRuntime::new(config.docker_binary.clone()));
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(config: DockerProvisionerConfig, runtime: DynDockerRuntime) -> Self {
        Self::new_with_runtime_and_optional_object_storage(config, runtime, None)
    }

    pub fn new_with_runtime_and_object_storage(
        config: DockerProvisionerConfig,
        runtime: DynDockerRuntime,
        object_storage: DynObjectStorage,
    ) -> Self {
        Self::new_with_runtime_and_optional_object_storage(config, runtime, Some(object_storage))
    }

    fn new_with_runtime_and_optional_object_storage(
        config: DockerProvisionerConfig,
        runtime: DynDockerRuntime,
        object_storage: Option<DynObjectStorage>,
    ) -> Self {
        Self {
            config,
            runtime,
            object_storage,
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn instance(&self, instance_id: &str) -> Option<HermesInstance> {
        self.instances.lock().ok()?.get(instance_id).cloned()
    }

    pub fn prepare_instance(&self, user_id: &str) -> HermesInstance {
        self.build_instance(user_id, false)
    }

    pub fn prepare_instance_with_sandbox(
        &self,
        user_id: &str,
        sandbox_enabled: bool,
    ) -> HermesInstance {
        self.build_instance(user_id, sandbox_enabled)
    }

    pub fn apply_sandbox_policy(&self, instance: &mut HermesInstance, sandbox_enabled: bool) {
        let user_root = self.config.data_root.join(&instance.user_id);
        // workspace/config 也按当前 data_root 归一化，避免旧部署路径继续残留在 DB 里。
        instance.host_workspace_path = Some(path_to_string(user_root.join("workspace")));
        instance.host_config_path = Some(path_to_string(user_root.join("config")));
        instance.host_sandbox_path =
            sandbox_enabled.then(|| path_to_string(user_root.join("sandbox")));
    }

    pub fn container_spec_for(
        &self,
        instance: &HermesInstance,
    ) -> Result<ContainerSpec, ProvisionerError> {
        let workspace = instance
            .host_workspace_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config = instance
            .host_config_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config_file = path_to_string(PathBuf::from(&config).join("config.yaml"));

        let mut mounts = vec![
            ContainerMount::bind(workspace, "/workspace", false),
            ContainerMount::bind(config, "/config", false),
            // Hermes 运行态目录保持可写，但 Hub 生成的配置文件本身必须只读挂载，
            // 这样管理员配置只能从 Hub/S3 更新，不会被容器内进程意外覆盖。
            ContainerMount::bind(config_file, "/config/config.yaml", true),
        ];
        if let Some(sandbox) = instance.host_sandbox_path.clone() {
            // 公共平台 Hermes 通过独立 sandbox 暴露临时会话数据目录；普通用户不挂载这组路径。
            mounts.insert(1, ContainerMount::bind(sandbox.clone(), "/sandbox", false));
            mounts.insert(2, ContainerMount::bind(sandbox, "/opt/data", false));
        }
        if let Some(managed_skills) = &self.config.managed_skills {
            mounts.push(ContainerMount::nfs_volume(
                managed_skills,
                !instance.global_skills_write_enabled,
            ));
        }
        let hub_nfs_dir = HUB_NFS_CONTAINER_DIR;
        let mut env = vec![
            "API_SERVER_ENABLED=true".to_string(),
            // API server 仅作为容器内本地能力保留；Hub 通信全部由 adapter 主动连接。
            "API_SERVER_HOST=127.0.0.1".to_string(),
            format!("API_SERVER_PORT={}", self.config.internal_port),
            format!(
                "API_SERVER_KEY={}",
                instance.llm_api_key.as_deref().unwrap_or("unissued")
            ),
            "HERMES_HOME=/config".to_string(),
            "HERMES_INFERENCE_PROVIDER=custom".to_string(),
            format!("CUSTOM_BASE_URL={}", self.config.hub_llm_base_url),
            format!("OPENAI_BASE_URL={}", self.config.hub_llm_base_url),
            format!(
                "OPENAI_API_KEY={}",
                instance.llm_api_key.as_deref().unwrap_or("unissued")
            ),
            format!(
                "HERMES_HUB_CHANNEL_BASE_URL={}",
                hub_channel_base_url(&self.config.hub_llm_base_url)
            ),
            format!(
                "HERMES_HUB_CHANNEL_TOKEN={}",
                instance.llm_api_key.as_deref().unwrap_or("unissued")
            ),
            format!("HERMES_HUB_INSTANCE_ID={}", instance.id),
            format!("HERMES_HUB_USER_ID={}", instance.user_id),
            format!("HERMES_HUB_NFS_DIR={hub_nfs_dir}"),
            format!("HERMES_HUB_INBOX_PATH={HUB_INBOX_PATH}"),
            format!("HERMES_HUB_INBOX_TIMEOUT_SECONDS={HUB_INBOX_TIMEOUT_SECONDS}"),
            format!("HERMES_HUB_INBOX_LIMIT={HUB_INBOX_LIMIT}"),
            format!(
                "HERMES_MEDIA_ALLOW_DIRS={}",
                media_allow_dirs_for_instance(instance)
            ),
            format!("OPENAI_MODEL={}", self.config.default_model),
            format!("HERMES_RUNTIME_IMAGE={}", self.config.image),
            format!(
                "HERMES_RUNTIME_VERSION={}",
                runtime_version_from_image(&self.config.image).unwrap_or_default()
            ),
            "HERMES_TOOL_PROGRESS_MODE=verbose".to_string(),
            // Hub 托管 Hermes 已经运行在用户独立容器里，命令安全边界由容器承担；
            // 默认自动批准可以避免长任务卡在无人值守的 approval prompt。
            "HERMES_YOLO_MODE=1".to_string(),
            "HERMES_ACCEPT_HOOKS=1".to_string(),
        ];
        if public_platform_cron_disabled(instance) {
            // 公共平台不提供长期定时任务能力；wrapper 内的 platform plugin 会让 cron tick 变为 no-op。
            env.push("HERMES_HUB_DISABLE_CRON=1".to_string());
        }
        if let Some(home_session_id) = instance.home_session_id.as_deref() {
            // Hermes 官方 send_message 的裸平台目标会落到 home_channel；Hub 固定为系统主会话，
            // 避免不同会话并发时通过进程级动态变量串到错误目标。
            env.push(format!("HERMES_HUB_HOME_CHANNEL={home_session_id}"));
        }
        if self.config.image_model_enabled {
            // 只有管理员显式启用图片模型时，才把图片生成模型暴露给 Hermes。
            env.push(format!("OPENAI_IMAGE_MODEL={}", self.config.image_model));
        }

        Ok(ContainerSpec {
            name: instance.name.clone(),
            image: self.config.image.clone(),
            network: self.config.network.clone(),
            internal_port: self.config.internal_port,
            env,
            // Hermes gateway 会在 HERMES_HOME 下写入 sessions、logs、skills 等运行态文件；
            // 统一管理 skills 通过单独只读挂载提供，避免进入 /config/skills 的 curator 路径。
            mounts,
            labels: vec![
                ("app".to_string(), "hermes-hub".to_string()),
                ("user_id".to_string(), instance.user_id.clone()),
                ("instance_id".to_string(), instance.id.clone()),
                (
                    MANAGED_CONTAINER_SPEC_LABEL.to_string(),
                    MANAGED_CONTAINER_SPEC_VERSION.to_string(),
                ),
            ],
            memory_limit: self.config.memory_limit.clone(),
            cpu_limit: self.config.cpu_limit.clone(),
            workdir: Some("/workspace".to_string()),
            healthcheck: Some(ContainerHealthcheck {
                // healthcheck 同时验证 Hermes 本地 gateway 和 Hub 内网地址；
                // 这样“进程还在但 adapter 连不上 Hub”不会继续被误判为健康。
                command: format!(
                    "fetch() {{ if command -v curl >/dev/null 2>&1; then curl -fsS --max-time 5 \"$1\" >/dev/null; elif command -v wget >/dev/null 2>&1; then wget -q -T 5 -O /dev/null \"$1\"; else exit 1; fi; }}; fetch \"http://127.0.0.1:{}/health\" && hub=\"${{HERMES_HUB_CHANNEL_BASE_URL%/internal/channel/v1}}\" && fetch \"$hub/health\"",
                    self.config.internal_port
                ),
                interval: "10s".to_string(),
                timeout: "6s".to_string(),
                retries: 6,
                // Hermes 冷启动时要初始化 s6、技能和 gateway；远端机器上 20s 容易误判为 unhealthy。
                start_period: "60s".to_string(),
            }),
            command: hermes_gateway_command(self.config.managed_profile.as_ref()),
        })
    }

    pub async fn ensure_container(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;
        self.ensure_managed_profile_files().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;

        let existing_inspection = self.inspect_container(&next.name).await?;
        let mut runtime_repair_only = false;
        if let Some(inspection) = existing_inspection.as_ref() {
            let config_changed = self.managed_config_changed(&next)?;
            let runtime_state_needs_repair = self.managed_runtime_state_needs_repair(&next)?;
            let current_container_usable = inspection.running
                && inspection.health_status.as_deref() != Some("unhealthy")
                && inspection.spec_version.as_deref() == Some(MANAGED_CONTAINER_SPEC_VERSION);
            if inspection.running
                && inspection.health_status.as_deref() != Some("unhealthy")
                && !config_changed
                && !runtime_state_needs_repair
                && inspection.spec_version.as_deref() == Some(MANAGED_CONTAINER_SPEC_VERSION)
            {
                apply_inspection_status(&mut next, &inspection);
                self.remember(next.clone())?;
                return Ok(next);
            }
            runtime_repair_only =
                current_container_usable && !config_changed && runtime_state_needs_repair;
        }

        let was_running = existing_inspection
            .as_ref()
            .map(|inspection| inspection.running)
            .unwrap_or(false);
        if was_running {
            self.run_required(vec!["stop".to_string(), next.name.clone()])
                .await?;
        }
        let write_result = self.write_managed_config(&next).await;
        if write_result.is_err() {
            if was_running {
                let _ = self
                    .run_required(vec!["start".to_string(), next.name.clone()])
                    .await;
            }
            return write_result.map(|_| next);
        }
        if runtime_repair_only {
            if was_running {
                self.run_required(vec!["start".to_string(), next.name.clone()])
                    .await?;
            }
            if let Some(inspection) = existing_inspection.as_ref() {
                apply_inspection_status(&mut next, inspection);
            }
            next.health_status = "starting".to_string();
            self.remember(next.clone())?;
            return Ok(next);
        }
        if existing_inspection.is_some() {
            // 旧版本可能创建了交互式 CLI、只读 /config 或发布宿主机端口的容器；
            // 模型配置变化时也需要重建，保证 gateway 读取 Hub 管理的 config.yaml。
            self.remove_container_if_exists(&next.name).await?;
        }
        let container_id = self.create_container(&next).await?;
        if let Err(error) = self
            .run_required(vec!["start".to_string(), next.name.clone()])
            .await
        {
            // create 已成功但 start 失败时不能留下 Created 半成品容器，否则后续管理页会显示混乱状态。
            let _ = self.remove_container_if_exists(&next.name).await;
            return Err(error);
        }
        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.health_status = "starting".to_string();
        next.status_message = None;
        self.remember(next.clone())?;

        Ok(next)
    }

    pub async fn ensure_container_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }
        provisioner.ensure_container(instance, llm_api_key).await
    }

    pub async fn write_config_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<bool, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        provisioner.create_host_directories(&next)?;
        let config_changed = provisioner.managed_config_changed(&next)?;
        let runtime_state_needs_repair = provisioner.managed_runtime_state_needs_repair(&next)?;
        if !config_changed && !runtime_state_needs_repair {
            return Ok(false);
        }
        // 只刷新 Hub 管理的配置文件，不重建 Docker 容器；写入期间先停 gateway，
        // 避免运行中的容器进程并发替换 /config 下的路径。
        let was_running = provisioner
            .inspect_container(&next.name)
            .await?
            .map(|inspection| inspection.running)
            .unwrap_or(false);
        if was_running {
            provisioner
                .run_required(vec!["stop".to_string(), next.name.clone()])
                .await?;
        }
        let write_result = provisioner.write_managed_config(&next).await;
        if was_running {
            let start_result = provisioner
                .run_required(vec!["start".to_string(), next.name.clone()])
                .await;
            if write_result.is_ok() {
                start_result?;
            } else {
                let _ = start_result;
            }
        }
        write_result.map(|_| false)
    }

    pub async fn refresh_instance_status(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        let mut next = instance.clone();
        match self.inspect_container(&instance.name).await? {
            Some(inspection) => apply_inspection_status(&mut next, &inspection),
            None => {
                next.container_id = None;
                next.status = HermesInstanceStatus::Error;
                next.health_status = "missing".to_string();
                next.status_message = Some("Docker container is missing".to_string());
            }
        }
        self.remember(next.clone())?;
        Ok(next)
    }

    pub async fn rebuild_instance_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }
        provisioner.rebuild_instance(instance, llm_api_key).await
    }

    fn build_instance(&self, user_id: &str, sandbox_enabled: bool) -> HermesInstance {
        let user_root = self.config.data_root.join(user_id);
        let workspace = user_root.join("workspace");
        let sandbox = user_root.join("sandbox");
        let config = user_root.join("config");

        let mut instance = HermesInstance::managed_docker(
            user_id,
            path_to_string(workspace),
            sandbox_enabled.then(|| path_to_string(sandbox)),
            path_to_string(config),
        );
        apply_runtime_image(&mut instance, &self.config.image);
        instance
    }

    fn ensure_managed(&self, instance: &HermesInstance) -> Result<(), ProvisionerError> {
        if instance.kind != HermesInstanceKind::ManagedDocker {
            return Err(ProvisionerError::InvalidManagedInstance);
        }

        Ok(())
    }

    fn create_host_directories(&self, instance: &HermesInstance) -> Result<(), ProvisionerError> {
        for path in [
            instance.host_workspace_path.as_deref(),
            instance.host_sandbox_path.as_deref(),
            instance.host_config_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            std::fs::create_dir_all(path)
                .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
        }

        for path in [
            instance.host_workspace_path.as_deref(),
            instance.host_sandbox_path.as_deref(),
            instance.host_config_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            set_directory_mode_for_container_tools(path)?;
        }

        Ok(())
    }

    async fn ensure_network(&self) -> Result<(), ProvisionerError> {
        let inspected = self
            .runtime
            .run(vec![
                "network".to_string(),
                "inspect".to_string(),
                self.config.network.clone(),
            ])
            .await?;

        if inspected.success {
            return Ok(());
        }

        self.run_required(vec![
            "network".to_string(),
            "create".to_string(),
            self.config.network.clone(),
        ])
        .await?;
        Ok(())
    }

    async fn inspect_container(
        &self,
        name: &str,
    ) -> Result<Option<ContainerInspection>, ProvisionerError> {
        let output = self
            .runtime
            .run(vec![
                "container".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                "{{json .}}".to_string(),
                name.to_string(),
            ])
            .await?;

        if output.success && !output.stdout.is_empty() {
            Ok(parse_container_inspection(&output.stdout))
        } else {
            Ok(None)
        }
    }

    async fn create_container(
        &self,
        instance: &HermesInstance,
    ) -> Result<String, ProvisionerError> {
        let spec = self.container_spec_for(instance)?;
        self.ensure_image_available(&spec.image).await?;
        self.ensure_nfs_volumes(&spec.mounts).await?;
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            spec.name.clone(),
            "--network".to_string(),
            spec.network.clone(),
            "--restart".to_string(),
            "always".to_string(),
        ];

        if let Some(workdir) = spec.workdir {
            args.push("--workdir".to_string());
            args.push(workdir);
        }

        for (key, value) in spec.labels {
            args.push("--label".to_string());
            args.push(format!("{key}={value}"));
        }
        for env in spec.env {
            args.push("--env".to_string());
            args.push(env);
        }
        for mount in spec.mounts {
            args.push("--mount".to_string());
            args.push(render_container_mount(&mount));
        }
        if let Some(memory_limit) = spec.memory_limit {
            args.push("--memory".to_string());
            args.push(memory_limit);
        }
        if let Some(cpu_limit) = spec.cpu_limit {
            args.push("--cpus".to_string());
            args.push(cpu_limit);
        }
        if let Some(healthcheck) = spec.healthcheck {
            args.push("--health-cmd".to_string());
            args.push(healthcheck.command);
            args.push("--health-interval".to_string());
            args.push(healthcheck.interval);
            args.push("--health-timeout".to_string());
            args.push(healthcheck.timeout);
            args.push("--health-retries".to_string());
            args.push(healthcheck.retries.to_string());
            args.push("--health-start-period".to_string());
            args.push(healthcheck.start_period);
        }
        args.push(spec.image);
        args.extend(spec.command);

        let output = self.run_required(args).await?;
        Ok(output.stdout.lines().next().unwrap_or_default().to_string())
    }

    async fn ensure_image_available(&self, image: &str) -> Result<(), ProvisionerError> {
        let inspected = self
            .runtime
            .run(vec![
                "image".to_string(),
                "inspect".to_string(),
                image.to_string(),
            ])
            .await?;
        if inspected.success {
            return Ok(());
        }

        // docker create 不会自动拉镜像；这里兜底拉取，避免管理员首次点击创建直接失败。
        self.run_required(vec!["pull".to_string(), image.to_string()])
            .await?;
        Ok(())
    }

    async fn ensure_nfs_volumes(&self, mounts: &[ContainerMount]) -> Result<(), ProvisionerError> {
        for mount in mounts {
            let ContainerMount::NfsVolume(mount) = mount else {
                continue;
            };
            self.run_required(vec![
                "volume".to_string(),
                "create".to_string(),
                "--driver".to_string(),
                "local".to_string(),
                "--opt".to_string(),
                "type=nfs".to_string(),
                "--opt".to_string(),
                format!("o={}", nfs_mount_options(&mount.addr, mount.read_only)),
                "--opt".to_string(),
                format!("device=:{}", normalize_nfs_export(&mount.export)),
                mount.volume_name.clone(),
            ])
            .await?;
        }
        Ok(())
    }

    async fn ensure_managed_profile_files(&self) -> Result<(), ProvisionerError> {
        let Some(profile) = &self.config.managed_profile else {
            return Ok(());
        };
        let Some(object_storage) = &self.object_storage else {
            return Ok(());
        };

        for file_name in [MANAGED_PROFILE_SOUL_FILE] {
            let key = managed_profile_object_key(&profile.object_prefix, file_name);
            match object_storage.get(&key).await {
                Ok(_) => {}
                Err(ObjectStorageError::NotFound) => {
                    // 首次创建用户 Hermes 前先放一个空文件，保证容器内符号链接可解析。
                    object_storage
                        .put(&key, Bytes::new())
                        .await
                        .map_err(|error| ProvisionerError::ObjectStorage(error.to_string()))?;
                }
                Err(error) => return Err(ProvisionerError::ObjectStorage(error.to_string())),
            }
        }
        Ok(())
    }

    async fn run_required(
        &self,
        args: Vec<String>,
    ) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let output = self.runtime.run(args).await?;

        if output.success {
            Ok(output)
        } else {
            Err(ProvisionerError::DockerCommand(
                if output.stderr.is_empty() {
                    output.stdout
                } else {
                    output.stderr
                },
            ))
        }
    }

    async fn remove_container_if_exists(&self, name: &str) -> Result<(), ProvisionerError> {
        let output = self
            .runtime
            .run(vec!["rm".to_string(), "-f".to_string(), name.to_string()])
            .await?;

        if output.success || output.stderr.contains("No such container") {
            Ok(())
        } else {
            Err(ProvisionerError::DockerCommand(
                if output.stderr.is_empty() {
                    output.stdout
                } else {
                    output.stderr
                },
            ))
        }
    }

    fn remember(&self, instance: HermesInstance) -> Result<(), ProvisionerError> {
        self.instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?
            .insert(instance.id.clone(), instance);
        Ok(())
    }

    async fn write_managed_config(
        &self,
        instance: &HermesInstance,
    ) -> Result<bool, ProvisionerError> {
        let config_path = managed_config_path(instance)?;
        let content = self.render_managed_config(instance)?;
        prepare_hermes_hub_pairing_paths(&config_path)?;
        let config_file = config_path.join("config.yaml");
        remove_symlink_if_exists(&config_file)?;
        let mut changed = write_file_if_changed(&config_file, &content)?;
        if let Some(object_storage) = &self.object_storage {
            object_storage
                .put(
                    &user_config_object_key(&instance.user_id),
                    Bytes::from(content.clone()),
                )
                .await
                .map_err(|error| ProvisionerError::ObjectStorage(error.to_string()))?;
        }
        // Hub adapter 现在随 wrapper 镜像作为 bundled platform plugin 提供；
        // 清理旧版写入 /config 的用户插件，避免它覆盖 /opt/hermes/plugins 下的新实现。
        changed |= remove_path_if_exists(&config_path.join("plugins/platforms/hermes_hub"))?;
        if public_platform_cron_disabled(instance) {
            // 公共平台数据可回收；清理旧任务，避免升级前创建的 cron 在禁用前残留。
            changed |= remove_path_if_exists(&config_path.join("cron"))?;
        }
        // pairing 是 Hermes gateway 的运行态授权数据，不属于容器规格。
        // 写入失败必须阻断编排；写入成功不需要为了状态文件变化而重建正在运行的容器。
        ensure_hermes_hub_pairing(&config_path, &instance.user_id)?;
        Ok(changed)
    }

    fn managed_config_changed(&self, instance: &HermesInstance) -> Result<bool, ProvisionerError> {
        let config_path = managed_config_path(instance)?;
        let config_file = config_path.join("config.yaml");
        if std::fs::symlink_metadata(&config_file)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Ok(true);
        }
        let content = self.render_managed_config(instance)?;
        Ok(std::fs::read_to_string(&config_file).ok().as_deref() != Some(content.as_str()))
    }

    fn managed_runtime_state_needs_repair(
        &self,
        instance: &HermesInstance,
    ) -> Result<bool, ProvisionerError> {
        let config_path = managed_config_path(instance)?;
        if std::fs::symlink_metadata(config_path.join("plugins/platforms/hermes_hub")).is_ok() {
            return Ok(true);
        }
        for pairing_dir in [
            config_path.join("pairing"),
            config_path.join("platforms/pairing"),
        ] {
            if pairing_directory_needs_repair(&config_path, &pairing_dir)? {
                return Ok(true);
            }
            let approved_path = pairing_dir.join("hermes_hub-approved.json");
            if pairing_approved_file_needs_repair(&approved_path, &instance.user_id)? {
                return Ok(true);
            }
            let pending_path = pairing_dir.join("hermes_hub-pending.json");
            if pairing_pending_file_needs_repair(&pending_path, &instance.user_id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn render_managed_config(&self, instance: &HermesInstance) -> Result<String, ProvisionerError> {
        let model = yaml_string(&self.config.default_model)?;
        let image_gen_section = if self.config.image_model_enabled {
            let image_model = yaml_string(&self.config.image_model)?;
            format!(
                "image_gen:\n\
                 \x20\x20provider: \"openai\"\n\
                 \x20\x20model: {image_model}\n\
                 \x20\x20openai:\n\
                 \x20\x20\x20\x20model: {image_model}\n"
            )
        } else {
            String::new()
        };
        let base_url = yaml_string(&self.config.hub_llm_base_url)?;
        let channel_base_url = yaml_string(&hub_channel_base_url(&self.config.hub_llm_base_url))?;
        let api_key = yaml_string(instance.llm_api_key.as_deref().unwrap_or(""))?;
        let api_mode = yaml_string(normalize_hermes_api_mode(&self.config.api_mode))?;
        let instance_id = yaml_string(&instance.id)?;
        let user_id = yaml_string(&instance.user_id)?;
        let media_allow_dirs_section = media_allow_dirs_yaml_section(instance)?;
        let platform_toolsets_section = platform_toolsets_yaml_section(instance)?;
        let cron_mode = yaml_string(if public_platform_cron_disabled(instance) {
            "deny"
        } else {
            "approve"
        })?;
        let managed_skills_section = self
            .config
            .managed_skills
            .as_ref()
            .map(|_| {
                Ok(format!(
                    "skills:\n  external_dirs:\n    - {}\n",
                    yaml_string(MANAGED_SKILLS_EXTERNAL_DIR)?
                ))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(format!(
            "# Managed by Hermes Hub. Do not edit model settings inside this container.\n\
             {managed_skills_section}\
             memory:\n\
             \x20\x20provider: holographic\n\
             plugins:\n\
             \x20\x20hermes-memory-store:\n\
             \x20\x20\x20\x20db_path: \"$HERMES_HOME/memory_store.db\"\n\
             \x20\x20\x20\x20default_trust: 0.5\n\
             \x20\x20\x20\x20hrr_dim: 1024\n\
             \x20\x20\x20\x20auto_extract: false\n\
             {platform_toolsets_section}\
             model:\n\
             \x20\x20default: {model}\n\
             \x20\x20provider: \"custom\"\n\
             \x20\x20base_url: {base_url}\n\
             \x20\x20api_key: {api_key}\n\
             \x20\x20api_mode: {api_mode}\n\
             \x20\x20context_window_tokens: {context_window_tokens}\n\
             \x20\x20max_output_tokens: {max_output_tokens}\n\
             \x20\x20temperature: {temperature}\n\
             \x20\x20parallel_tool_calls: {parallel_tool_calls}\n\
             {image_gen_section}\
             display:\n\
             \x20\x20tool_progress: \"verbose\"\n\
             \x20\x20tool_progress_command: true\n\
             \x20\x20platforms:\n\
             \x20\x20\x20\x20api_server:\n\
             \x20\x20\x20\x20\x20\x20tool_progress: \"verbose\"\n\
             \x20\x20\x20\x20\x20\x20tool_preview_length: 0\n\
             \x20\x20\x20\x20hermes_hub:\n\
             \x20\x20\x20\x20\x20\x20tool_progress: \"verbose\"\n\
             \x20\x20\x20\x20\x20\x20tool_preview_length: 0\n\
             auxiliary:\n\
             \x20\x20session_search:\n\
             \x20\x20\x20\x20provider: \"main\"\n\
             \x20\x20\x20\x20timeout: 60\n\
             \x20\x20\x20\x20max_concurrency: 1\n\
             gateway:\n\
             {media_allow_dirs_section}\
             \x20\x20platforms:\n\
             \x20\x20\x20\x20hermes_hub:\n\
             \x20\x20\x20\x20\x20\x20enabled: true\n\
             \x20\x20\x20\x20\x20\x20extra:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20base_url: {channel_base_url}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20inbox_path: \"{HUB_INBOX_PATH}\"\n\
             \x20\x20\x20\x20\x20\x20\x20\x20instance_id: {instance_id}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20user_id: {user_id}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20timeout_seconds: {HUB_INBOX_TIMEOUT_SECONDS}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20limit: {HUB_INBOX_LIMIT}\n\
             approvals:\n\
             \x20\x20mode: \"off\"\n\
             \x20\x20timeout: 3600\n\
             \x20\x20cron_mode: {cron_mode}\n\
             \x20\x20mcp_reload_confirm: false\n\
             \x20\x20destructive_slash_confirm: false\n",
            context_window_tokens = self.config.context_window_tokens,
            max_output_tokens = self.config.max_output_tokens,
            temperature = self.config.temperature,
            parallel_tool_calls = self.config.supports_parallel_tools,
        ))
    }
}

fn user_config_object_key(user_id: &str) -> String {
    // 用户 id 来自 Hub 数据库；这里只兜底去掉对象存储路径分隔符，避免非预期 key 层级。
    let safe_user_id = user_id
        .chars()
        .map(|ch| if ch == '/' || ch == '\\' { '_' } else { ch })
        .collect::<String>();
    format!("config/users/{safe_user_id}/config.yaml")
}

fn media_allow_dirs_for_instance(instance: &HermesInstance) -> &'static str {
    let _ = instance;
    // Hermes 已经能读取容器内文件；这里保持附件发送能力与读文件能力一致。
    HERMES_MEDIA_ALLOW_DIRS
}

fn media_allow_dirs_yaml_section(instance: &HermesInstance) -> Result<String, ProvisionerError> {
    let _ = instance;
    let dirs = [HERMES_MEDIA_ALLOW_DIRS];

    let mut section = "  media_delivery_allow_dirs:\n".to_string();
    for dir in dirs {
        section.push_str(&format!("    - {}\n", yaml_string(dir)?));
    }
    Ok(section)
}

fn public_platform_cron_disabled(instance: &HermesInstance) -> bool {
    instance.host_sandbox_path.is_some()
}

fn platform_toolsets_yaml_section(instance: &HermesInstance) -> Result<String, ProvisionerError> {
    let mut toolsets = vec![
        "web",
        "browser",
        "terminal",
        "file",
        "code_execution",
        "vision",
        "image_gen",
        "skills",
        "todo",
        "memory",
        "session_search",
        "clarify",
        "delegation",
    ];
    if !public_platform_cron_disabled(instance) {
        // 普通托管 Hermes 仍保留用户定时任务能力；公共平台会话会被回收，不支持长期任务。
        toolsets.push("cronjob");
    }
    toolsets.push("hermes_hub");

    let mut section = "platform_toolsets:\n  hermes_hub:\n".to_string();
    for toolset in toolsets {
        section.push_str(&format!("    - {}\n", yaml_string(toolset)?));
    }
    Ok(section)
}

fn parse_container_inspection(raw: &str) -> Option<ContainerInspection> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let id = value
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if id.is_empty() {
        return None;
    }
    let state = value.get("State").and_then(Value::as_object);
    let running = state
        .and_then(|state| state.get("Running"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let health_status = state
        .and_then(|state| state.get("Health"))
        .and_then(|health| health.get("Status"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let health_error = state
        .and_then(|state| state.get("Health"))
        .and_then(docker_health_error);
    let spec_version = value
        .get("Config")
        .and_then(|config| config.get("Labels"))
        .and_then(|labels| labels.get(MANAGED_CONTAINER_SPEC_LABEL))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let image = value
        .get("Config")
        .and_then(|config| config.get("Image"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Some(ContainerInspection {
        id,
        running,
        health_status,
        health_error,
        spec_version,
        image,
    })
}

fn docker_health_error(health: &Value) -> Option<String> {
    health
        .get("Log")
        .and_then(Value::as_array)
        .and_then(|logs| logs.iter().rev().find_map(docker_health_log_error))
}

fn docker_health_log_error(log: &Value) -> Option<String> {
    let output = log
        .get("Output")
        .and_then(Value::as_str)
        .map(clean_status_message)
        .filter(|output| !output.is_empty());
    if output.is_some() {
        return output;
    }
    log.get("ExitCode")
        .and_then(Value::as_i64)
        .filter(|code| *code != 0)
        .map(|code| format!("Docker healthcheck exited with code {code}"))
}

fn clean_status_message(message: &str) -> String {
    // Docker health log 可能包含多行 curl/wget 输出；前端只需要可读摘要，避免撑爆表格。
    message.trim().chars().take(1024).collect()
}

fn apply_inspection_status(instance: &mut HermesInstance, inspection: &ContainerInspection) {
    instance.container_id = Some(inspection.id.clone());
    if let Some(image) = &inspection.image {
        apply_runtime_image(instance, image);
    }
    if !inspection.running {
        instance.status = HermesInstanceStatus::Stopped;
        instance.health_status = "stopped".to_string();
        instance.status_message = None;
        return;
    }

    match inspection.health_status.as_deref() {
        Some("healthy") => {
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = "healthy".to_string();
            instance.status_message = None;
        }
        Some("starting") => {
            instance.status = HermesInstanceStatus::Provisioning;
            instance.health_status = "starting".to_string();
            instance.status_message = None;
        }
        Some("unhealthy") => {
            instance.status = HermesInstanceStatus::Error;
            instance.health_status = "unhealthy".to_string();
            instance.status_message = inspection
                .health_error
                .clone()
                .or_else(|| Some("Docker healthcheck reported unhealthy".to_string()));
        }
        Some(other) => {
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = other.to_string();
            instance.status_message = None;
        }
        None => {
            // 兼容旧容器：没有 Docker healthcheck 时只能确认进程运行。
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = "running".to_string();
            instance.status_message = None;
        }
    }
}

fn apply_runtime_image(instance: &mut HermesInstance, image: &str) {
    // 镜像 tag 是 adapter 尚未上报前的兜底；一旦容器内上报了真实版本，就不再覆盖。
    let previous_image_version = instance
        .runtime_image
        .as_deref()
        .and_then(runtime_version_from_image);
    let next_image_version = runtime_version_from_image(image);
    instance.runtime_image = Some(image.to_string());
    if instance.runtime_version.is_none() || instance.runtime_version == previous_image_version {
        instance.runtime_version = next_image_version;
    }
}

fn runtime_version_from_image(image: &str) -> Option<String> {
    let image_without_digest = image.split('@').next().unwrap_or(image);
    let last_segment = image_without_digest
        .rsplit('/')
        .next()
        .unwrap_or(image_without_digest);
    last_segment
        .rsplit_once(':')
        .map(|(_, tag)| tag.trim())
        .filter(|tag| !tag.is_empty() && *tag != "latest")
        .map(ToOwned::to_owned)
}

#[async_trait]
impl HermesProvisioner for DockerProvisioner {
    async fn ensure_instance(
        &self,
        user_id: &str,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let instance = self.build_instance(user_id, false);
        self.ensure_container(&instance, llm_api_key).await
    }

    async fn start_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        let Some(inspection) = self.inspect_container(&instance.name).await? else {
            return Err(ProvisionerError::InstanceNotFound);
        };
        self.run_required(vec!["start".to_string(), instance.name.clone()])
            .await?;

        let mut next = instance.clone();
        next.container_id = Some(inspection.id);
        next.status = HermesInstanceStatus::Running;
        next.health_status = "starting".to_string();
        next.status_message = None;
        self.remember(next.clone())?;
        Ok(next)
    }

    async fn stop_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        if self.inspect_container(&instance.name).await?.is_none() {
            return Err(ProvisionerError::InstanceNotFound);
        }
        self.run_required(vec!["stop".to_string(), instance.name.clone()])
            .await?;

        let mut next = instance.clone();
        next.status = HermesInstanceStatus::Stopped;
        next.health_status = "stopped".to_string();
        next.status_message = None;
        self.remember(next.clone())?;
        Ok(next)
    }

    async fn rebuild_instance(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;
        self.ensure_managed_profile_files().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        let existing_inspection = self.inspect_container(&next.name).await?;
        let was_running = existing_inspection
            .as_ref()
            .map(|inspection| inspection.running)
            .unwrap_or(false);
        if was_running {
            self.run_required(vec!["stop".to_string(), next.name.clone()])
                .await?;
        }
        let write_result = self.write_managed_config(&next).await;
        if write_result.is_err() {
            if was_running {
                let _ = self
                    .run_required(vec!["start".to_string(), next.name.clone()])
                    .await;
            }
            return write_result.map(|_| next);
        }
        if existing_inspection.is_some() {
            self.remove_container_if_exists(&next.name).await?;
        }
        let container_id = self.create_container(&next).await?;
        if let Err(error) = self
            .run_required(vec!["start".to_string(), next.name.clone()])
            .await
        {
            // 重建流程里 create/start 不是原子操作；start 失败时主动回收，避免残留 Created 容器。
            let _ = self.remove_container_if_exists(&next.name).await;
            return Err(error);
        }

        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.health_status = "starting".to_string();
        next.status_message = None;
        self.remember(next.clone())?;
        Ok(next)
    }
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn render_container_mount(mount: &ContainerMount) -> String {
    let mut value = match mount {
        ContainerMount::Bind(mount) => {
            format!(
                "type=bind,src={},dst={}",
                mount.host_path, mount.container_path
            )
        }
        ContainerMount::NfsVolume(mount) => {
            format!(
                "type=volume,src={},dst={},volume-driver=local",
                mount.volume_name, mount.container_path
            )
        }
    };
    if mount.read_only() {
        value.push_str(",readonly");
    }
    value
}

fn hermes_gateway_command(managed_profile: Option<&ManagedProfileConfig>) -> Vec<String> {
    if managed_profile.is_some() {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "exec /opt/hermes/.venv/bin/hermes gateway".to_string(),
        ]
    } else {
        vec!["gateway".to_string()]
    }
}

fn managed_profile_object_key(prefix: &str, file_name: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        file_name.to_string()
    } else {
        format!("{prefix}/{file_name}")
    }
}

fn nfs_mount_options(addr: &str, read_only: bool) -> String {
    let (host, port) = split_nfs_addr(addr);
    let mode = if read_only { "ro" } else { "rw" };
    format!(
        "addr={host},port={port},mountport={port},vers=3,tcp,nolock,soft,actimeo=0,lookupcache=none,{mode}"
    )
}

fn managed_nfs_volume_name(base_name: &str, read_only: bool) -> String {
    if read_only {
        // Docker 不会更新已有 volume 的 NFS options；换名确保新容器使用禁缓存挂载。
        format!("{base_name}-live")
    } else {
        // rw 挂载也要独立名称，避免复用普通用户 ro volume 以及旧缓存参数。
        format!("{base_name}-rw-live")
    }
}

fn split_nfs_addr(addr: &str) -> (&str, &str) {
    addr.rsplit_once(':').unwrap_or((addr, "2049"))
}

fn normalize_nfs_export(export: &str) -> String {
    let trimmed = export.trim();
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn normalize_hermes_api_mode(api_mode: &str) -> &str {
    if api_mode == RESPONSES_API_TYPE {
        // Hermes 内部用 codex_responses 表示 OpenAI Responses API。
        "codex_responses"
    } else {
        api_mode
    }
}

fn hub_channel_base_url(hub_llm_base_url: &str) -> String {
    let trimmed = hub_llm_base_url.trim_end_matches('/');
    if trimmed.ends_with("/internal/channel/v1") {
        return trimmed.to_string();
    }

    if let Some(base) = trimmed.strip_suffix("/internal/llm/v1") {
        return format!("{base}/internal/channel/v1");
    }

    if let Ok(mut url) = reqwest::Url::parse(trimmed) {
        // channel API 是 Hub 自己的内部协议，始终挂在 Hub origin 下；
        // 即便管理员只配置了 http://host:port，也要派生出完整 internal channel 前缀。
        url.set_path("/internal/channel/v1");
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string().trim_end_matches('/').to_string();
    }

    trimmed.to_string()
}

fn write_file_if_changed(path: &Path, content: &str) -> Result<bool, ProvisionerError> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    }
    std::fs::write(path, content)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    Ok(true)
}

fn managed_config_path(instance: &HermesInstance) -> Result<PathBuf, ProvisionerError> {
    instance
        .host_config_path
        .as_ref()
        .map(PathBuf::from)
        .ok_or(ProvisionerError::InvalidManagedInstance)
}

fn remove_symlink_if_exists(path: &Path) -> Result<bool, ProvisionerError> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }
    std::fs::remove_file(path).map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    Ok(true)
}

fn remove_path_if_exists(path: &Path) -> Result<bool, ProvisionerError> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    } else {
        std::fs::remove_file(path)
            .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    }
    Ok(true)
}

fn ensure_hermes_hub_pairing(config_path: &Path, user_id: &str) -> Result<(), ProvisionerError> {
    let approved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default();

    // Hermes 当前通过 get_hermes_dir("platforms/pairing", "pairing") 解析目录：
    // 旧目录存在时继续使用 /pairing，新安装则偏向 /platforms/pairing。
    // Hub 同时写两份，保证已有用户和后续 Hermes 目录升级都不会重新触发配对。
    for pairing_dir in [
        config_path.join("pairing"),
        config_path.join("platforms/pairing"),
    ] {
        let approved_path = pairing_dir.join("hermes_hub-approved.json");
        let pending_path = pairing_dir.join("hermes_hub-pending.json");
        ensure_pairing_directory_tree(config_path, &pairing_dir)?;
        ensure_pairing_file_path(&approved_path)?;
        ensure_pairing_file_path(&pending_path)?;
        ensure_approved_pairing(&approved_path, user_id, approved_at)?;
        ensure_pairing_permissions_for_container(config_path, &pairing_dir, &approved_path)?;
        clear_pending_pairing_for_user(&pending_path, user_id)?;
        set_pairing_file_mode_for_container(&pending_path)?;
    }

    Ok(())
}

fn prepare_hermes_hub_pairing_paths(config_path: &Path) -> Result<(), ProvisionerError> {
    for pairing_dir in [
        config_path.join("pairing"),
        config_path.join("platforms/pairing"),
    ] {
        ensure_pairing_directory_tree(config_path, &pairing_dir)?;
        ensure_pairing_file_path(&pairing_dir.join("hermes_hub-approved.json"))?;
        ensure_pairing_file_path(&pairing_dir.join("hermes_hub-pending.json"))?;
    }
    Ok(())
}

fn ensure_approved_pairing(
    approved_path: &Path,
    user_id: &str,
    approved_at: f64,
) -> Result<(), ProvisionerError> {
    let mut approved = read_json_object(approved_path)?;
    if approved.contains_key(user_id) {
        return Ok(());
    }

    approved.insert(
        user_id.to_string(),
        json!({
            "user_name": "Hub user",
            "approved_at": approved_at,
        }),
    );
    write_json_object_if_changed(approved_path, &approved)?;
    Ok(())
}

fn clear_pending_pairing_for_user(
    pending_path: &Path,
    user_id: &str,
) -> Result<(), ProvisionerError> {
    if !pending_path.exists() {
        return Ok(());
    }

    let mut pending = read_json_object(pending_path)?;
    let before_len = pending.len();
    pending.retain(|_, entry| entry.get("user_id").and_then(Value::as_str) != Some(user_id));

    if pending.len() != before_len {
        write_json_object_if_changed(pending_path, &pending)?;
    }

    Ok(())
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, ProvisionerError> {
    if !path.exists() {
        return Ok(Map::new());
    }

    let content = std::fs::read_to_string(path)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;

    if content.trim().is_empty() {
        return Ok(Map::new());
    }

    match serde_json::from_str::<Value>(&content) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) | Err(_) => Ok(Map::new()),
    }
}

fn write_json_object_if_changed(
    path: &Path,
    object: &Map<String, Value>,
) -> Result<bool, ProvisionerError> {
    let content = serde_json::to_string_pretty(object)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    write_file_if_changed(path, &content)
}

#[cfg(unix)]
fn set_pairing_directory_mode_for_container(path: &Path) -> Result<(), ProvisionerError> {
    // pairing 相关目录由 Hub 在宿主机上创建；即便 approved JSON 是 0644，
    // 任意一层父目录如果保持 root:root 0700，Hermes gateway 进程也无法进入读取。
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProvisionerError::Filesystem(format!(
            "pairing path is not a real directory: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}

#[cfg(not(unix))]
fn set_pairing_directory_mode_for_container(_path: &Path) -> Result<(), ProvisionerError> {
    Ok(())
}

fn ensure_pairing_permissions_for_container(
    config_path: &Path,
    pairing_dir: &Path,
    approved_path: &Path,
) -> Result<(), ProvisionerError> {
    ensure_path_within_config(config_path, pairing_dir)?;
    if let Some(parent) = pairing_dir.parent() {
        if parent != config_path {
            ensure_path_within_config(config_path, parent)?;
            set_pairing_directory_mode_for_container(parent)?;
        }
    }
    ensure_path_within_config(config_path, approved_path)?;
    set_pairing_directory_mode_for_container(pairing_dir)?;
    set_pairing_file_mode_for_container(approved_path)
}

fn ensure_pairing_directory_tree(
    config_path: &Path,
    pairing_dir: &Path,
) -> Result<(), ProvisionerError> {
    ensure_real_directory(config_path)?;
    if let Some(parent) = pairing_dir.parent() {
        if parent != config_path {
            ensure_real_directory(parent)?;
            ensure_path_within_config(config_path, parent)?;
        }
    }
    ensure_real_directory(pairing_dir)?;
    ensure_path_within_config(config_path, pairing_dir)
}

fn ensure_real_directory(path: &Path) -> Result<(), ProvisionerError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(ProvisionerError::Filesystem(format!(
                "pairing path is not a real directory: {}",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
            ensure_real_directory(path)
        }
        Err(error) => Err(ProvisionerError::Filesystem(error.to_string())),
    }
}

fn ensure_pairing_file_path(path: &Path) -> Result<(), ProvisionerError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(ProvisionerError::Filesystem(format!(
                "pairing path is not a real file: {}",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ProvisionerError::Filesystem(error.to_string())),
    }
}

fn ensure_path_within_config(config_path: &Path, path: &Path) -> Result<(), ProvisionerError> {
    let config_root = std::fs::canonicalize(config_path)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    if !resolved.starts_with(&config_root) {
        return Err(ProvisionerError::Filesystem(format!(
            "pairing path escapes config directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn pairing_directory_needs_repair(
    config_path: &Path,
    pairing_dir: &Path,
) -> Result<bool, ProvisionerError> {
    let Some(parent) = pairing_dir.parent() else {
        return Ok(true);
    };
    if parent != config_path && real_directory_needs_repair(config_path, parent, 0o755)? {
        return Ok(true);
    }
    real_directory_needs_repair(config_path, pairing_dir, 0o755)
}

fn real_directory_needs_repair(
    config_path: &Path,
    path: &Path,
    expected_mode: u32,
) -> Result<bool, ProvisionerError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(ProvisionerError::Filesystem(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(true);
    }
    ensure_path_within_config(config_path, path)?;
    Ok(unix_mode_needs_repair(&metadata, expected_mode))
}

fn pairing_approved_file_needs_repair(
    path: &Path,
    user_id: &str,
) -> Result<bool, ProvisionerError> {
    if real_file_needs_repair(path, 0o644)? {
        return Ok(true);
    }
    let approved = read_json_object(path)?;
    Ok(!approved.contains_key(user_id))
}

fn pairing_pending_file_needs_repair(path: &Path, user_id: &str) -> Result<bool, ProvisionerError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ProvisionerError::Filesystem(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(true);
    }
    if unix_mode_needs_repair(&metadata, 0o644) {
        return Ok(true);
    }
    let pending = read_json_object(path)?;
    Ok(pending
        .values()
        .any(|entry| entry.get("user_id").and_then(Value::as_str) == Some(user_id)))
}

fn real_file_needs_repair(path: &Path, expected_mode: u32) -> Result<bool, ProvisionerError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(ProvisionerError::Filesystem(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(true);
    }
    Ok(unix_mode_needs_repair(&metadata, expected_mode))
}

#[cfg(unix)]
fn unix_mode_needs_repair(metadata: &std::fs::Metadata, expected_mode: u32) -> bool {
    metadata.permissions().mode() & 0o777 != expected_mode
}

#[cfg(not(unix))]
fn unix_mode_needs_repair(_metadata: &std::fs::Metadata, _expected_mode: u32) -> bool {
    false
}

#[cfg(unix)]
fn set_pairing_file_mode_for_container(path: &Path) -> Result<(), ProvisionerError> {
    // approved/pending 文件可能来自旧版 Hermes 或 Hub，不能假设已有权限可被
    // gateway 用户读取；这里只放开读取，不授予额外写权限。
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ProvisionerError::Filesystem(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProvisionerError::Filesystem(format!(
            "pairing path is not a real file: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}

#[cfg(not(unix))]
fn set_pairing_file_mode_for_container(_path: &Path) -> Result<(), ProvisionerError> {
    Ok(())
}

#[cfg(unix)]
fn set_directory_mode_for_container_tools(path: &str) -> Result<(), ProvisionerError> {
    // Hermes gateway 进程以容器内 hermes 用户运行，Hub 在宿主机创建的目录默认是 root:root。
    // 第一版先把工具输出目录设为可写，避免 npm/pip/文件生成类任务卡在 EACCES。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}

#[cfg(not(unix))]
fn set_directory_mode_for_container_tools(_path: &str) -> Result<(), ProvisionerError> {
    Ok(())
}

fn yaml_string(value: &str) -> Result<String, ProvisionerError> {
    serde_json::to_string(value).map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}

#[cfg(test)]
mod command_docker_runtime_tests {
    use std::{ffi::OsStr, path::PathBuf};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn parses_docker_daemon_api_version_from_socket_http_response() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            r#"{"Version":"24.0.7","ApiVersion":"1.43","MinAPIVersion":"1.12"}"#
        );

        assert_eq!(
            parse_docker_api_version_response(response),
            Some("1.43".to_string())
        );
    }

    #[test]
    fn docker_runtime_only_retries_client_too_new_errors() {
        assert!(docker_client_api_is_too_new(
            "Error response from daemon: client version 1.52 is too new. Maximum supported API version is 1.43"
        ));
        assert!(!docker_client_api_is_too_new(
            "Error response from daemon: client version 1.41 is too old. Minimum supported API version is 1.44"
        ));
    }

    #[test]
    fn command_runtime_treats_blank_docker_api_version_as_unset() {
        assert!(!docker_api_version_env_is_explicit(None));
        assert!(!docker_api_version_env_is_explicit(Some(OsStr::new(""))));
        assert_eq!(
            docker_api_version_env_is_explicit(Some(OsStr::new("1.44"))),
            true
        );
    }

    #[cfg(unix)]
    #[test]
    fn docker_socket_path_tracks_unix_docker_host() {
        assert_eq!(
            docker_socket_path_from_env(None),
            Some(PathBuf::from(DOCKER_DAEMON_SOCKET))
        );
        assert_eq!(
            docker_socket_path_from_env(Some(OsStr::new("unix:///tmp/docker.sock"))),
            Some(PathBuf::from("/tmp/docker.sock"))
        );
        assert_eq!(
            docker_socket_path_from_env(Some(OsStr::new("tcp://127.0.0.1:2375"))),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn command_runtime_injects_cached_api_version_into_child_process() {
        let temp_dir = tempfile::tempdir().expect("temporary fake docker dir is created");
        let docker_path = temp_dir.path().join("docker");
        std::fs::write(
            &docker_path,
            "#!/bin/sh\nprintf '%s' \"$DOCKER_API_VERSION\"\n",
        )
        .expect("fake docker binary is written");
        let mut permissions = std::fs::metadata(&docker_path)
            .expect("fake docker metadata exists")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&docker_path, permissions)
            .expect("fake docker binary is executable");

        let runtime = CommandDockerRuntime::new_with_cached_docker_api_version(
            docker_path.to_string_lossy().to_string(),
            Some("1.43".to_string()),
        );

        let output = runtime
            .run(vec!["version".to_string()])
            .await
            .expect("fake docker command runs");

        assert!(output.success);
        assert_eq!(output.stdout, "1.43");
    }
}
