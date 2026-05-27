use async_trait::async_trait;
use thiserror::Error;

use super::instance::HermesInstance;

#[derive(Debug, Error)]
pub enum ProvisionerError {
    #[error("instance not found")]
    InstanceNotFound,
    #[error("invalid managed instance")]
    InvalidManagedInstance,
    #[error("provisioner lock failed")]
    LockFailed,
    #[error("filesystem operation failed: {0}")]
    Filesystem(String),
    #[error("object storage operation failed: {0}")]
    ObjectStorage(String),
    #[error("docker runtime failed: {0}")]
    DockerRuntime(String),
    #[error("docker command failed: {0}")]
    DockerCommand(String),
}

/// Hermes 实例编排抽象。v1 实现 Docker，未来可替换为独立 provisioner 或 K8s。
#[async_trait]
pub trait HermesProvisioner {
    async fn ensure_instance(
        &self,
        user_id: &str,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError>;

    async fn start_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError>;

    async fn stop_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError>;

    async fn rebuild_instance(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError>;
}
