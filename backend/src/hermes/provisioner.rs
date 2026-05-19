use std::future::Future;

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
}

/// Hermes 实例编排抽象。v1 实现 Docker，未来可替换为独立 provisioner 或 K8s。
pub trait HermesProvisioner {
    fn ensure_instance(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<HermesInstance, ProvisionerError>> + Send;

    fn start_instance(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<(), ProvisionerError>> + Send;

    fn stop_instance(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<(), ProvisionerError>> + Send;

    fn rebuild_instance(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<HermesInstance, ProvisionerError>> + Send;
}
