use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::Serialize;
use tokio::process::Command;

use super::{
    instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    provisioner::{HermesProvisioner, ProvisionerError},
};

/// Docker 托管 Hermes 的运行配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockerProvisionerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub hub_llm_base_url: String,
    pub default_model: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
}

/// 容器挂载定义。测试和真实 Docker adapter 共用同一份 spec，避免部署行为漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// 可渲染为 Docker create 参数的规范。这里显式保存 published_ports，
/// 用测试保证托管 Hermes 不暴露宿主机端口。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub internal_port: u16,
    pub env: Vec<String>,
    pub mounts: Vec<ContainerMount>,
    pub labels: Vec<(String, String)>,
    pub published_ports: Vec<String>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
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
}

impl CommandDockerRuntime {
    pub fn new(docker_binary: String) -> Self {
        Self { docker_binary }
    }
}

#[async_trait]
impl DockerRuntime for CommandDockerRuntime {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let output = Command::new(&self.docker_binary)
            .args(&args)
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
            "noop-container-id".to_string()
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
    instances: Arc<Mutex<HashMap<String, HermesInstance>>>,
}

impl DockerProvisioner {
    pub fn new(config: DockerProvisionerConfig) -> Self {
        let runtime = Arc::new(CommandDockerRuntime::new(config.docker_binary.clone()));
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(config: DockerProvisionerConfig, runtime: DynDockerRuntime) -> Self {
        Self {
            config,
            runtime,
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn instance(&self, instance_id: &str) -> Option<HermesInstance> {
        self.instances.lock().ok()?.get(instance_id).cloned()
    }

    pub fn prepare_instance(&self, user_id: &str) -> HermesInstance {
        self.build_instance(user_id)
    }

    pub fn container_spec_for(
        &self,
        instance: &HermesInstance,
    ) -> Result<ContainerSpec, ProvisionerError> {
        let workspace = instance
            .host_workspace_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let sandbox = instance
            .host_sandbox_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config = instance
            .host_config_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;

        Ok(ContainerSpec {
            name: instance.name.clone(),
            image: self.config.image.clone(),
            network: self.config.network.clone(),
            internal_port: self.config.internal_port,
            env: vec![
                "API_SERVER_ENABLED=true".to_string(),
                format!("OPENAI_BASE_URL={}", self.config.hub_llm_base_url),
                format!(
                    "OPENAI_API_KEY={}",
                    instance.llm_api_key.as_deref().unwrap_or("unissued")
                ),
                format!("OPENAI_MODEL={}", self.config.default_model),
            ],
            mounts: vec![
                ContainerMount {
                    host_path: workspace,
                    container_path: "/workspace".to_string(),
                    read_only: false,
                },
                ContainerMount {
                    host_path: sandbox,
                    container_path: "/sandbox".to_string(),
                    read_only: false,
                },
                ContainerMount {
                    host_path: config,
                    container_path: "/config".to_string(),
                    read_only: true,
                },
            ],
            labels: vec![
                ("app".to_string(), "hermes-hub".to_string()),
                ("user_id".to_string(), instance.user_id.clone()),
                ("instance_id".to_string(), instance.id.clone()),
            ],
            published_ports: Vec::new(),
            memory_limit: self.config.memory_limit.clone(),
            cpu_limit: self.config.cpu_limit.clone(),
        })
    }

    pub async fn ensure_container(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;

        if let Some(container_id) = self.inspect_container(&next.name).await? {
            self.run_required(vec!["start".to_string(), next.name.clone()])
                .await?;
            next.container_id = Some(container_id);
            next.status = HermesInstanceStatus::Running;
            self.remember(next.clone())?;
            return Ok(next);
        }

        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;
        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        self.remember(next.clone())?;

        Ok(next)
    }

    fn build_instance(&self, user_id: &str) -> HermesInstance {
        let container_name = managed_container_name(user_id);
        let user_root = self.config.data_root.join(user_id);
        let workspace = user_root.join("workspace");
        let sandbox = user_root.join("sandbox");
        let config = user_root.join("config");

        HermesInstance::managed_docker(
            user_id,
            format!("http://{container_name}:{}", self.config.internal_port),
            path_to_string(workspace),
            path_to_string(sandbox),
            path_to_string(config),
        )
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

    async fn inspect_container(&self, name: &str) -> Result<Option<String>, ProvisionerError> {
        let output = self
            .runtime
            .run(vec![
                "container".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                name.to_string(),
            ])
            .await?;

        if output.success && !output.stdout.is_empty() {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    async fn create_container(
        &self,
        instance: &HermesInstance,
    ) -> Result<String, ProvisionerError> {
        let spec = self.container_spec_for(instance)?;
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            spec.name.clone(),
            "--network".to_string(),
            spec.network.clone(),
        ];

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
            let mut value = format!(
                "type=bind,src={},dst={}",
                mount.host_path, mount.container_path
            );
            if mount.read_only {
                value.push_str(",readonly");
            }
            args.push(value);
        }
        if let Some(memory_limit) = spec.memory_limit {
            args.push("--memory".to_string());
            args.push(memory_limit);
        }
        if let Some(cpu_limit) = spec.cpu_limit {
            args.push("--cpus".to_string());
            args.push(cpu_limit);
        }

        args.push(spec.image);

        let output = self.run_required(args).await?;
        Ok(output.stdout.lines().next().unwrap_or_default().to_string())
    }

    async fn run_required(
        &self,
        args: Vec<String>,
    ) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let output = self.runtime.run(args).await?;

        if output.success {
            Ok(output)
        } else {
            Err(ProvisionerError::DockerCommand(if output.stderr.is_empty() {
                output.stdout
            } else {
                output.stderr
            }))
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
            Err(ProvisionerError::DockerCommand(if output.stderr.is_empty() {
                output.stdout
            } else {
                output.stderr
            }))
        }
    }

    fn remember(&self, instance: HermesInstance) -> Result<(), ProvisionerError> {
        self.instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?
            .insert(instance.id.clone(), instance);
        Ok(())
    }
}

#[async_trait]
impl HermesProvisioner for DockerProvisioner {
    async fn ensure_instance(
        &self,
        user_id: &str,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let instance = self.build_instance(user_id);
        self.ensure_container(&instance, llm_api_key).await
    }

    async fn start_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        let Some(container_id) = self.inspect_container(&instance.name).await? else {
            return Err(ProvisionerError::InstanceNotFound);
        };
        self.run_required(vec!["start".to_string(), instance.name.clone()])
            .await?;

        let mut next = instance.clone();
        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
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

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        self.remove_container_if_exists(&next.name).await?;
        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;

        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        self.remember(next.clone())?;
        Ok(next)
    }
}

fn managed_container_name(user_id: &str) -> String {
    format!("hermes-user-{user_id}")
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}
