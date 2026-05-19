use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use super::{
    instance::{HermesInstance, HermesInstanceStatus},
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
}

/// 容器挂载定义。测试和真实 Docker adapter 共用同一份 spec，避免部署行为漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// 可渲染为 Docker create/run 参数的规范。这里显式保存 published_ports，
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

/// Docker provisioner 的 MVP 实现。
///
/// 当前实现负责生成稳定实例记录和容器 spec；真实 Docker daemon 调用会在部署集成阶段接入。
#[derive(Clone)]
pub struct DockerProvisioner {
    config: DockerProvisionerConfig,
    instances: Arc<Mutex<HashMap<String, HermesInstance>>>,
}

impl DockerProvisioner {
    pub fn new(config: DockerProvisionerConfig) -> Self {
        Self {
            config,
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn instance(&self, instance_id: &str) -> Option<HermesInstance> {
        self.instances.lock().ok()?.get(instance_id).cloned()
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
                format!("LLM_BASE_URL={}", self.config.hub_llm_base_url),
                "LLM_API_KEY_FILE=/config/instance-token".to_string(),
                format!("DEFAULT_MODEL={}", self.config.default_model),
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
}

impl HermesProvisioner for DockerProvisioner {
    async fn ensure_instance(&self, user_id: &str) -> Result<HermesInstance, ProvisionerError> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?;

        if let Some(instance) = instances.values().find(|item| item.user_id == user_id) {
            return Ok(instance.clone());
        }

        let instance = self.build_instance(user_id);
        instances.insert(instance.id.clone(), instance.clone());
        Ok(instance)
    }

    async fn start_instance(&self, instance_id: &str) -> Result<(), ProvisionerError> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?;
        let instance = instances
            .get_mut(instance_id)
            .ok_or(ProvisionerError::InstanceNotFound)?;

        instance.status = HermesInstanceStatus::Running;
        Ok(())
    }

    async fn stop_instance(&self, instance_id: &str) -> Result<(), ProvisionerError> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?;
        let instance = instances
            .get_mut(instance_id)
            .ok_or(ProvisionerError::InstanceNotFound)?;

        instance.status = HermesInstanceStatus::Stopped;
        Ok(())
    }

    async fn rebuild_instance(
        &self,
        instance_id: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?;
        let instance = instances
            .get_mut(instance_id)
            .ok_or(ProvisionerError::InstanceNotFound)?;

        // 重建只替换容器，不删除用户 workspace/sandbox/config 挂载目录。
        instance.status = HermesInstanceStatus::Running;
        Ok(instance.clone())
    }
}

fn managed_container_name(user_id: &str) -> String {
    format!("hermes-user-{user_id}")
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}
