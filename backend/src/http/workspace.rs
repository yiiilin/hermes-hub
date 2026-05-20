use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::{
    hermes::instance::{HermesInstance, HermesInstanceKind},
    http::{auth::current_user, ApiError},
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
}

#[derive(Serialize)]
struct WorkspaceStatusResponse {
    hermes_instance: Option<HermesInstance>,
}

#[derive(Serialize)]
struct HermesInstanceResponse {
    hermes_instance: HermesInstance,
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let hermes_instance = state.store.hermes_instance_for_user(&user.id).await.ok();

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
    if let Ok(instance) = state.store.hermes_instance_for_user(user_id).await {
        if instance.kind != HermesInstanceKind::ManagedDocker {
            return Ok(instance);
        }

        ensure_required_model_configs(state).await?;
        let llm_config = state
            .model_registry
            .active_config()
            .await
            .map_err(|_| ApiError::Internal)?;
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
                &llm_config.default_model,
                &llm_config.api_type,
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        state
            .store
            .bind_hermes_instance(ensured.clone())
            .await
            .map_err(|_| ApiError::Internal)?;

        return Ok(ensured);
    }

    ensure_required_model_configs(state).await?;
    let llm_config = state
        .model_registry
        .active_config()
        .await
        .map_err(|_| ApiError::Internal)?;
    let instance = state.docker_provisioner.prepare_instance(user_id);
    state
        .store
        .bind_hermes_instance(instance.clone())
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
            &llm_config.default_model,
            &llm_config.api_type,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(instance)
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

    Ok(Json(HermesInstanceResponse { hermes_instance }))
}
