use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::user::{PublicUser, UserListItem},
    hermes::instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    http::{auth::require_admin, ApiError},
    model_config::ModelConfig,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{user_id}/disable", post(disable_user))
        .route("/api/admin/users/{user_id}/enable", post(enable_user))
        .route("/api/admin/hermes-instances", get(list_hermes_instances))
        .route(
            "/api/admin/users/{user_id}/hermes-instance/bind-external",
            post(bind_external_hermes_instance),
        )
        .route(
            "/api/admin/users/{user_id}/hermes-instance/rebuild-managed",
            post(rebuild_managed_hermes_instance),
        )
        .route(
            "/api/admin/users/{user_id}/hermes-instance/stop",
            post(stop_managed_hermes_instance),
        )
        .route(
            "/api/admin/users/{user_id}/hermes-instance/start",
            post(start_managed_hermes_instance),
        )
        .route(
            "/api/admin/model-config",
            get(get_model_config).put(update_model_config),
        )
}

#[derive(Serialize)]
struct UserListResponse {
    users: Vec<UserListItem>,
}

#[derive(Serialize)]
struct UserResponse {
    user: PublicUser,
}

#[derive(Serialize)]
struct HermesInstancesResponse {
    hermes_instances: Vec<HermesInstance>,
}

#[derive(Serialize)]
struct HermesInstanceResponse {
    hermes_instance: HermesInstance,
}

#[derive(Serialize)]
struct ModelConfigResponse {
    model_config: ModelConfig,
}

#[derive(Deserialize)]
struct UpdateModelConfigRequest {
    provider_name: String,
    provider_base_url: String,
    provider_api_key: String,
    default_model: String,
    allowed_models: Vec<String>,
    allow_streaming: bool,
    request_timeout_seconds: u64,
}

#[derive(Deserialize)]
struct BindExternalHermesRequest {
    name: String,
    base_url: String,
    api_token: Option<String>,
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let users = state.store.list_users().map_err(|_| ApiError::Internal)?;
    Ok(Json(UserListResponse { users }))
}

async fn disable_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = require_admin(&state, &headers)?;
    if admin.id == user_id {
        return Err(ApiError::Conflict("admin cannot disable own account"));
    }

    let user = state
        .store
        .disable_user(&user_id)
        .map_err(|_| ApiError::NotFound("user not found"))?;
    Ok(Json(UserResponse {
        user: user.public(),
    }))
}

async fn enable_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let user = state
        .store
        .enable_user(&user_id)
        .map_err(|_| ApiError::NotFound("user not found"))?;
    Ok(Json(UserResponse {
        user: user.public(),
    }))
}

async fn list_hermes_instances(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let hermes_instances = state
        .store
        .list_hermes_instances()
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(HermesInstancesResponse { hermes_instances }))
}

async fn bind_external_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(payload): Json<BindExternalHermesRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let instance = HermesInstance {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user_id.clone(),
        kind: HermesInstanceKind::External,
        status: HermesInstanceStatus::Running,
        name: payload.name,
        base_url: payload.base_url,
        api_token_secret_ref: payload.api_token,
        container_id: None,
        host_workspace_path: None,
        host_sandbox_path: None,
        host_config_path: None,
        health_status: "unknown".to_string(),
    };
    state
        .store
        .bind_hermes_instance(instance)
        .map_err(|_| ApiError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn rebuild_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let instance = state
        .store
        .set_hermes_instance_status(&user_id, HermesInstanceStatus::Running)
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn stop_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let instance = state
        .store
        .set_hermes_instance_status(&user_id, HermesInstanceStatus::Stopped)
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn start_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    let instance = state
        .store
        .set_hermes_instance_status(&user_id, HermesInstanceStatus::Running)
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn get_model_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(ModelConfigResponse {
        model_config: state
            .model_registry
            .active_config()
            .map_err(|_| ApiError::Internal)?,
    }))
}

async fn update_model_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateModelConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers)?;
    state.model_registry.replace(ModelConfig {
        provider_name: payload.provider_name,
        provider_base_url: payload.provider_base_url,
        provider_api_key: payload.provider_api_key,
        default_model: payload.default_model,
        allowed_models: payload.allowed_models,
        allow_streaming: payload.allow_streaming,
        request_timeout_seconds: payload.request_timeout_seconds,
    });

    Ok(StatusCode::NO_CONTENT)
}
