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
        instance::{HermesInstance, HermesInstanceKind},
    },
    http::{auth::current_user, map_provisioner_error, ApiError},
    model_config::IMAGE_MODEL_CONFIG_KIND,
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
    if let Ok(instance) = state.store.hermes_instance_for_user(user_id).await {
        if instance.kind == HermesInstanceKind::ManagedDocker {
            let mut instance = instance;
            instance.global_skills_write_enabled = global_skills_write_enabled;
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
            state
                .channel_store
                .bind_hub_channel_to_instance(user_id, &ensured.id)
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
    let mut instance = state.docker_provisioner.prepare_instance(user_id);
    instance.global_skills_write_enabled = global_skills_write_enabled;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .channel_store
        .bind_hub_channel_to_instance(user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
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
        .channel_store
        .bind_hub_channel_to_instance(user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;

    state
        .store
        .hermes_instance_for_user(user_id)
        .await
        .map_err(|_| ApiError::Internal)
}

async fn user_has_global_skills_write_access(
    state: &AppState,
    user_id: &str,
) -> Result<bool, ApiError> {
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
