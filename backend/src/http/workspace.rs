use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::{
    domain::user::UserRole,
    hermes::{
        docker_provisioner::RuntimeModelSettings,
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    },
    http::{auth::current_user, map_provisioner_error, ApiError},
    model_config::IMAGE_MODEL_CONFIG_KIND,
    public_platform,
    session::store::HermesSchedulerSnapshot,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/workspace/status", get(status))
        .route("/api/workspace/ensure-hermes", post(ensure_hermes))
        .route(
            "/api/workspace/hermes-instance",
            get(current_hermes_instance),
        )
        .route(
            "/api/workspace/hermes-scheduler-snapshot",
            get(current_scheduler_snapshot),
        )
}

const ADAPTER_HEARTBEAT_TTL_SECONDS: u64 = 45;
const HERMES_STARTUP_GRACE_SECONDS: u64 = 120;

#[derive(Serialize)]
struct WorkspaceStatusResponse {
    hermes_instance: Option<HermesInstance>,
}

#[derive(Serialize)]
struct HermesInstanceResponse {
    hermes_instance: HermesInstance,
}

#[derive(Serialize)]
struct WorkspaceSchedulerSnapshotResponse {
    hermes_scheduler_snapshot: Option<HermesSchedulerSnapshot>,
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let hermes_instance = match state.store.hermes_instance_for_user(&user.id).await {
        Ok(instance) => Some(refresh_managed_hermes_status(&state, instance).await?),
        Err(_) => None,
    };

    Ok(Json(WorkspaceStatusResponse { hermes_instance }))
}

async fn ensure_hermes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let instance = ensure_managed_hermes_for_user(&state, &user.id).await?;

    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

pub async fn ensure_required_model_configs(state: &AppState) -> Result<(), ApiError> {
    let readiness = state
        .model_registry
        .required_runtime_config_readiness()
        .await
        .map_err(|_| ApiError::Internal)?;

    if !readiness.ready {
        return Err(ApiError::Conflict(
            "available llm and title model configs are required before this operation",
        ));
    }

    Ok(())
}

pub async fn ensure_managed_hermes_for_user(
    state: &AppState,
    user_id: &str,
) -> Result<HermesInstance, ApiError> {
    ensure_managed_hermes_for_user_without_activity(state, user_id).await?;
    state
        .store
        .record_hermes_user_activity(user_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .store
        .hermes_instance_for_user(user_id)
        .await
        .map_err(|_| ApiError::Internal)
}

pub async fn ensure_managed_hermes_for_user_without_activity(
    state: &AppState,
    user_id: &str,
) -> Result<HermesInstance, ApiError> {
    let global_skills_write_enabled = user_has_global_skills_write_access(state, user_id).await?;
    let sandbox_enabled = public_platform::is_public_user_id(state, user_id).await?;
    if let Ok(instance) = state.store.hermes_instance_for_user(user_id).await {
        if instance.kind == HermesInstanceKind::ManagedDocker {
            let mut instance = instance;
            instance.global_skills_write_enabled = global_skills_write_enabled;
            state
                .docker_provisioner
                .apply_sandbox_policy(&mut instance, sandbox_enabled);
            ensure_home_session_for_hermes_instance(state, user_id, &mut instance).await?;
            ensure_required_model_configs(state).await?;
            let llm_config = state
                .model_registry
                .active_config()
                .await
                .map_err(|_| ApiError::Internal)?;
            let image_config = state
                .model_registry
                .config_for_kind(IMAGE_MODEL_CONFIG_KIND)
                .await
                .map_err(|_| ApiError::Internal)?;
            let image_model = image_config
                .enabled
                .then_some(image_config.default_model.as_str());
            // 数据库里的容器状态可能滞后于 Docker daemon；ensure 操作会幂等检查并启动容器。
            let llm_api_key = match instance.api_token_secret_ref.as_deref() {
                Some(existing_token) => {
                    state
                        .model_registry
                        .add_instance_token_for_instance(&instance.id, existing_token)
                        .await
                        .map_err(|_| ApiError::Internal)?;
                    existing_token.to_string()
                }
                None => state
                    .model_registry
                    .issue_instance_token_for_instance(&instance.id)
                    .await
                    .map_err(|_| ApiError::Internal)?,
            };
            let ensured = state
                .docker_provisioner
                .ensure_container_with_default_model(
                    &instance,
                    &llm_api_key,
                    &runtime_model_settings(&llm_config),
                    image_model,
                )
                .await
                .map_err(map_provisioner_error)?;
            state
                .store
                .bind_hermes_instance(ensured.clone())
                .await
                .map_err(|_| ApiError::Internal)?;

            return state
                .store
                .hermes_instance_for_user(user_id)
                .await
                .map_err(|_| ApiError::Internal);
        }
    }

    ensure_required_model_configs(state).await?;
    let llm_config = state
        .model_registry
        .active_config()
        .await
        .map_err(|_| ApiError::Internal)?;
    let image_config = state
        .model_registry
        .config_for_kind(IMAGE_MODEL_CONFIG_KIND)
        .await
        .map_err(|_| ApiError::Internal)?;
    let image_model = image_config
        .enabled
        .then_some(image_config.default_model.as_str());
    let mut instance = state
        .docker_provisioner
        .prepare_instance_with_sandbox(user_id, sandbox_enabled);
    instance.global_skills_write_enabled = global_skills_write_enabled;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    ensure_home_session_for_hermes_instance(state, user_id, &mut instance).await?;
    let llm_api_key = state
        .model_registry
        .issue_instance_token_for_instance(&instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let instance = state
        .docker_provisioner
        .ensure_container_with_default_model(
            &instance,
            &llm_api_key,
            &runtime_model_settings(&llm_config),
            image_model,
        )
        .await
        .map_err(map_provisioner_error)?;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;

    state
        .store
        .hermes_instance_for_user(user_id)
        .await
        .map_err(|_| ApiError::Internal)
}

pub(crate) async fn ensure_home_session_for_hermes_instance(
    state: &AppState,
    user_id: &str,
    instance: &mut HermesInstance,
) -> Result<String, ApiError> {
    let channel = state
        .channel_store
        .bind_hub_channel_to_instance(user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let session = state
        .channel_store
        .ensure_home_session(user_id, &channel.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    instance.home_session_id = Some(session.id.clone());
    Ok(session.id)
}

async fn user_has_global_skills_write_access(
    state: &AppState,
    user_id: &str,
) -> Result<bool, ApiError> {
    if public_platform::is_public_user_id(state, user_id).await? {
        return Ok(false);
    }

    let users = state
        .store
        .list_users()
        .await
        .map_err(|_| ApiError::Internal)?;
    users
        .iter()
        .find(|user| user.id == user_id)
        .map(|user| user.role == UserRole::Admin)
        .ok_or(ApiError::NotFound("user not found"))
}

pub async fn refresh_managed_hermes_status(
    state: &AppState,
    instance: HermesInstance,
) -> Result<HermesInstance, ApiError> {
    if instance.kind != HermesInstanceKind::ManagedDocker {
        return Ok(instance);
    }

    // UI 展示必须以 Docker daemon 的当前事实为准，不能只相信数据库里上次写入的状态。
    let refreshed = state
        .docker_provisioner
        .refresh_instance_status(&instance)
        .await
        .map_err(map_provisioner_error)?;
    let refreshed = apply_adapter_heartbeat_status(refreshed);
    state
        .store
        .bind_hermes_instance(refreshed.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .store
        .hermes_instance_for_user(&refreshed.user_id)
        .await
        .map_err(|_| ApiError::Internal)
}

fn apply_adapter_heartbeat_status(mut instance: HermesInstance) -> HermesInstance {
    let now = unix_now();
    let adapter_recent = instance
        .adapter_last_seen_at
        .is_some_and(|seen_at| now.saturating_sub(seen_at) <= ADAPTER_HEARTBEAT_TTL_SECONDS);
    if adapter_recent && matches_docker_process_alive(&instance) {
        // adapter 能连上 Hub 比 Docker healthcheck 更能证明 Hermes gateway 实际可用。
        instance.status = HermesInstanceStatus::Running;
        instance.health_status = "healthy".to_string();
        instance.status_message = None;
        return instance;
    }

    let in_startup_grace = instance
        .last_started_at
        .is_some_and(|started_at| now.saturating_sub(started_at) <= HERMES_STARTUP_GRACE_SECONDS);
    if in_startup_grace
        && matches!(instance.status, HermesInstanceStatus::Error)
        && instance.health_status == "unhealthy"
    {
        // Hermes 冷启动可能先连续几次 healthcheck 失败；启动宽限期内不要写死为 error。
        instance.status = HermesInstanceStatus::Provisioning;
        instance.health_status = "starting".to_string();
        instance.status_message = None;
        return instance;
    }

    if instance.adapter_last_seen_at.is_some()
        && !adapter_recent
        && matches_docker_process_alive(&instance)
    {
        instance.status = HermesInstanceStatus::Error;
        instance.health_status = "adapter_offline".to_string();
        instance.status_message = Some("Adapter heartbeat timed out".to_string());
    }

    instance
}

fn matches_docker_process_alive(instance: &HermesInstance) -> bool {
    !matches!(instance.status, HermesInstanceStatus::Stopped)
        && !matches!(instance.health_status.as_str(), "missing" | "stopped")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_instance() -> HermesInstance {
        HermesInstance::managed_docker(
            "user-1",
            "/tmp/hermes/user-1/workspace".to_string(),
            None,
            "/tmp/hermes/user-1/config".to_string(),
        )
    }

    #[test]
    fn recent_adapter_heartbeat_overrides_transient_docker_unhealthy() {
        let mut instance = test_instance();
        instance.status = HermesInstanceStatus::Error;
        instance.health_status = "unhealthy".to_string();
        instance.status_message = Some("curl: connection refused".to_string());
        instance.adapter_last_seen_at = Some(unix_now());

        let normalized = apply_adapter_heartbeat_status(instance);

        assert_eq!(normalized.status, HermesInstanceStatus::Running);
        assert_eq!(normalized.health_status, "healthy");
        assert_eq!(normalized.status_message, None);
    }

    #[test]
    fn startup_grace_keeps_transient_unhealthy_as_starting() {
        let mut instance = test_instance();
        instance.status = HermesInstanceStatus::Error;
        instance.health_status = "unhealthy".to_string();
        instance.status_message = Some("curl: connection refused".to_string());
        instance.last_started_at = Some(unix_now());

        let normalized = apply_adapter_heartbeat_status(instance);

        assert_eq!(normalized.status, HermesInstanceStatus::Provisioning);
        assert_eq!(normalized.health_status, "starting");
        assert_eq!(normalized.status_message, None);
    }

    #[test]
    fn stale_adapter_heartbeat_marks_running_container_offline() {
        let mut instance = test_instance();
        instance.status = HermesInstanceStatus::Running;
        instance.health_status = "healthy".to_string();
        instance.adapter_last_seen_at = Some(unix_now() - ADAPTER_HEARTBEAT_TTL_SECONDS - 1);

        let normalized = apply_adapter_heartbeat_status(instance);

        assert_eq!(normalized.status, HermesInstanceStatus::Error);
        assert_eq!(normalized.health_status, "adapter_offline");
        assert_eq!(
            normalized.status_message.as_deref(),
            Some("Adapter heartbeat timed out")
        );
    }
}

fn runtime_model_settings(config: &crate::model_config::ModelConfig) -> RuntimeModelSettings {
    RuntimeModelSettings {
        default_model: config.default_model.clone(),
        api_mode: config.api_type.clone(),
        context_window_tokens: config.context_window_tokens,
        max_output_tokens: config.max_output_tokens,
        temperature: config.temperature,
        supports_parallel_tools: config.supports_parallel_tools,
    }
}

async fn current_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let hermes_instance = state
        .store
        .hermes_instance_for_user(&user.id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    let hermes_instance = refresh_managed_hermes_status(&state, hermes_instance).await?;

    Ok(Json(HermesInstanceResponse { hermes_instance }))
}

async fn current_scheduler_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let snapshot = match state.store.hermes_instance_for_user(&user.id).await {
        Ok(instance) => state
            .store
            .hermes_scheduler_snapshot_for_instance(&instance.id)
            .await
            .map_err(|_| ApiError::Internal)?,
        // 新用户的 Hermes 可能还没上报过调度快照；个人页应展示空状态而不是报错。
        Err(_) => None,
    };

    Ok(Json(WorkspaceSchedulerSnapshotResponse {
        hermes_scheduler_snapshot: snapshot,
    }))
}
