use serde::Serialize;
use uuid::Uuid;

/// Hermes 实例来源。Hub 只创建并管理内置 adapter 的 Docker Hermes 容器。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HermesInstanceKind {
    ManagedDocker,
}

/// Hermes 实例运行状态。后续 HTTP API 和前端直接使用这组稳定状态。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HermesInstanceStatus {
    Provisioning,
    Running,
    Stopped,
    Error,
}

/// Hub 内部保存的 Hermes 实例记录。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HermesInstance {
    pub id: String,
    pub user_id: String,
    pub kind: HermesInstanceKind,
    pub status: HermesInstanceStatus,
    pub name: String,
    #[serde(skip_serializing)]
    pub api_token_secret_ref: Option<String>,
    #[serde(skip_serializing)]
    pub llm_api_key: Option<String>,
    pub container_id: Option<String>,
    pub host_workspace_path: Option<String>,
    pub host_sandbox_path: Option<String>,
    pub host_config_path: Option<String>,
    pub health_status: String,
}

impl HermesInstance {
    /// 为用户创建一个托管 Docker 实例记录，真实容器动作由 provisioner 执行。
    pub fn managed_docker(
        user_id: &str,
        host_workspace_path: String,
        host_sandbox_path: String,
        host_config_path: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            kind: HermesInstanceKind::ManagedDocker,
            status: HermesInstanceStatus::Running,
            name: format!("hermes-user-{user_id}"),
            api_token_secret_ref: None,
            llm_api_key: None,
            container_id: None,
            host_workspace_path: Some(host_workspace_path),
            host_sandbox_path: Some(host_sandbox_path),
            host_config_path: Some(host_config_path),
            health_status: "unknown".to_string(),
        }
    }
}
