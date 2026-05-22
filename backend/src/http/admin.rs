use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Instant;

use crate::{
    domain::user::{PublicUser, UserListItem},
    hermes::{
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
        provisioner::{HermesProvisioner, ProvisionerError},
    },
    http::{
        auth::require_admin,
        workspace::{ensure_managed_hermes_for_user, ensure_required_model_configs},
        ApiError,
    },
    llm_proxy::{LlmProviderError, LlmProviderRequest},
    model_config::{
        default_api_type_for_kind, validate_api_type_for_kind, ModelConfig,
        CHAT_COMPLETIONS_API_TYPE, IMAGE_MODEL_CONFIG_KIND, LLM_MODEL_CONFIG_KIND,
        RESPONSES_API_TYPE,
    },
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
            "/api/admin/users/{user_id}/hermes-instance/external-config",
            axum::routing::put(update_external_hermes_instance_config),
        )
        .route(
            "/api/admin/users/{user_id}/hermes-instance/create-managed",
            post(create_managed_hermes_instance),
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
        .route(
            "/api/admin/model-config/{config_kind}/test",
            post(test_model_config),
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
    model_configs: Vec<ModelConfig>,
    required_models_ready: bool,
    missing_required_model_config_kinds: Vec<String>,
}

#[derive(Deserialize)]
struct UpdateModelConfigRequest {
    config_kind: Option<String>,
    provider_name: String,
    provider_base_url: String,
    provider_api_key: String,
    default_model: String,
    allowed_models: Option<Vec<String>>,
    api_type: Option<String>,
    reasoning_effort: Option<String>,
    allow_streaming: bool,
    request_timeout_seconds: u64,
}

#[derive(Serialize)]
struct ModelConfigTestResponse {
    ok: bool,
    status_code: u16,
    message: String,
    duration_ms: u64,
}

#[derive(Deserialize)]
struct BindExternalHermesRequest {
    name: String,
    base_url: String,
    api_token: Option<String>,
}

#[derive(Deserialize)]
struct UpdateExternalHermesConfigRequest {
    name: String,
    base_url: String,
    api_token: Option<String>,
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let users = state
        .store
        .list_users()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(UserListResponse { users }))
}

async fn disable_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = require_admin(&state, &headers).await?;
    if admin.id == user_id {
        return Err(ApiError::Conflict("admin cannot disable own account"));
    }

    let user = state
        .store
        .disable_user(&user_id)
        .await
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
    require_admin(&state, &headers).await?;
    let user = state
        .store
        .enable_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("user not found"))?;
    Ok(Json(UserResponse {
        user: user.public(),
    }))
}

async fn list_hermes_instances(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let hermes_instances = state
        .store
        .list_hermes_instances()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(HermesInstancesResponse { hermes_instances }))
}

async fn bind_external_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(payload): Json<BindExternalHermesRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    ensure_user_exists(&state, &user_id).await?;
    let instance = HermesInstance {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user_id.clone(),
        kind: HermesInstanceKind::External,
        status: HermesInstanceStatus::Running,
        name: payload.name,
        base_url: payload.base_url,
        api_token_secret_ref: payload.api_token,
        llm_api_key: None,
        container_id: None,
        host_workspace_path: None,
        host_sandbox_path: None,
        host_config_path: None,
        health_status: "unknown".to_string(),
    };
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .channel_store
        .bind_hub_channel_to_instance(&user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn update_external_hermes_instance_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(payload): Json<UpdateExternalHermesConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_user_exists(&state, &user_id).await?;
    let mut instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;

    if instance.kind != HermesInstanceKind::External {
        return Err(ApiError::Conflict(
            "managed hermes runtime config is controlled by hub",
        ));
    }

    instance.name = payload.name;
    instance.base_url = payload.base_url;
    if let Some(api_token) = payload.api_token {
        // 空字符串表示沿用已保存 token，便于管理员只修改名称或地址。
        if !api_token.trim().is_empty() {
            instance.api_token_secret_ref = Some(api_token);
        }
    }
    instance.status = HermesInstanceStatus::Running;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .channel_store
        .bind_hub_channel_to_instance(&user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn create_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    ensure_user_exists(&state, &user_id).await?;
    if let Ok(instance) = state.store.hermes_instance_for_user(&user_id).await {
        reject_external_instance(&instance)?;
    }

    // 管理员补建和用户工作区 ensure 共用同一条幂等编排路径，避免两套 Docker 创建逻辑漂移。
    let instance = ensure_managed_hermes_for_user(&state, &user_id).await?;

    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn ensure_user_exists(state: &AppState, user_id: &str) -> Result<(), ApiError> {
    let users = state
        .store
        .list_users()
        .await
        .map_err(|_| ApiError::Internal)?;
    if users.iter().any(|user| user.id == user_id) {
        Ok(())
    } else {
        Err(ApiError::NotFound("user not found"))
    }
}

async fn rebuild_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    reject_external_instance(&instance)?;
    state
        .model_registry
        .revoke_instance_tokens_for_instance(&instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let llm_api_key = state
        .model_registry
        .issue_instance_token_for_instance(&instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
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
    let instance = state
        .docker_provisioner
        .rebuild_instance_with_default_model(
            &instance,
            &llm_api_key,
            &llm_config.default_model,
            &image_config.default_model,
            &llm_config.api_type,
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
        .bind_hub_channel_to_instance(&user_id, &instance.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn stop_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    reject_external_instance(&instance)?;
    let instance = state
        .docker_provisioner
        .stop_instance(&instance)
        .await
        .map_err(map_provisioner_error)?;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn start_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    reject_external_instance(&instance)?;
    let instance = ensure_managed_hermes_for_user(&state, &user_id).await?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn get_model_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let model_configs = state
        .model_registry
        .all_configs()
        .await
        .map_err(|_| ApiError::Internal)?;
    let model_config = model_configs
        .iter()
        .find(|config| config.config_kind == LLM_MODEL_CONFIG_KIND)
        .cloned()
        .ok_or(ApiError::Internal)?;
    let readiness = state
        .model_registry
        .required_runtime_config_readiness()
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(ModelConfigResponse {
        model_config,
        model_configs,
        required_models_ready: readiness.ready,
        missing_required_model_config_kinds: readiness.missing_config_kinds,
    }))
}

async fn update_model_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateModelConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let config_kind = payload.config_kind.clone();
    let config = model_config_from_payload(&state, config_kind.as_deref(), payload).await?;
    state
        .model_registry
        .replace(config)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn test_model_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(config_kind): Path<String>,
    Json(payload): Json<UpdateModelConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let config = model_config_from_payload(&state, Some(&config_kind), payload).await?;
    let (path, body) = model_test_request(&config)?;
    let request = LlmProviderRequest {
        method: Method::POST,
        provider_base_url: config.provider_base_url.clone(),
        path,
        authorization: format!("Bearer {}", config.provider_api_key),
        content_type: "application/json".to_string(),
        body,
        timeout_seconds: model_test_timeout_seconds(&config),
    };
    let started = Instant::now();
    let response = state
        .llm_provider
        .send(request)
        .await
        .map_err(map_model_test_provider_error)?;
    let status = response.status();
    let message = if status.is_success() {
        "model test succeeded".to_string()
    } else {
        let bytes = to_bytes(response.into_body(), 16 * 1024)
            .await
            .map_err(|_| ApiError::BadGateway("provider test response could not be read"))?;
        test_response_message(status, &bytes)
    };

    Ok(Json(ModelConfigTestResponse {
        ok: status.is_success(),
        status_code: status.as_u16(),
        message,
        duration_ms: started.elapsed().as_millis() as u64,
    }))
}

async fn model_config_from_payload(
    state: &AppState,
    config_kind: Option<&str>,
    payload: UpdateModelConfigRequest,
) -> Result<ModelConfig, ApiError> {
    let config_kind = config_kind
        .map(ToOwned::to_owned)
        .or(payload.config_kind)
        .unwrap_or_else(|| LLM_MODEL_CONFIG_KIND.to_string());
    let provider_api_key = if payload.provider_api_key.trim().is_empty() {
        // 空字符串表示沿用已保存密钥，方便管理员只改模型名或 Base URL。
        state
            .model_registry
            .config_for_kind(&config_kind)
            .await
            .map_err(|_| ApiError::BadRequest("invalid model config kind"))?
            .provider_api_key
    } else {
        payload.provider_api_key
    };

    Ok(ModelConfig {
        api_type: payload
            .api_type
            .unwrap_or_else(|| default_api_type_for_kind(&config_kind).to_string()),
        reasoning_effort: payload.reasoning_effort,
        config_kind,
        provider_name: payload.provider_name,
        provider_base_url: payload.provider_base_url,
        provider_api_key,
        default_model: payload.default_model,
        allowed_models: payload.allowed_models.unwrap_or_default(),
        allow_streaming: payload.allow_streaming,
        request_timeout_seconds: payload.request_timeout_seconds,
    })
}

fn model_test_request(config: &ModelConfig) -> Result<(String, Vec<u8>), ApiError> {
    validate_api_type_for_kind(&config.config_kind, &config.api_type)
        .map_err(|_| ApiError::BadRequest("invalid model api type"))?;
    let body = if config.config_kind == IMAGE_MODEL_CONFIG_KIND {
        json!({
            "model": config.default_model,
            "prompt": "Hermes Hub model connectivity test",
            "n": 1,
            "size": "1024x1024"
        })
    } else if config.api_type == RESPONSES_API_TYPE {
        let mut body = json!({
            "model": config.default_model,
            "input": "Reply with exactly: ok",
            "stream": false,
            "max_output_tokens": 8
        });
        if let Some(effort) = config.reasoning_effort.as_deref() {
            body["reasoning"] = json!({ "effort": effort });
        }
        body
    } else {
        let mut body = json!({
            "model": config.default_model,
            "messages": [
                {
                    "role": "user",
                    "content": "Reply with exactly: ok"
                }
            ],
            "stream": false,
            "max_tokens": 8
        });
        if let Some(effort) = config.reasoning_effort.as_deref() {
            body["reasoning_effort"] = json!(effort);
        }
        body
    };
    let path = if config.config_kind == IMAGE_MODEL_CONFIG_KIND {
        "/images/generations"
    } else if config.api_type == RESPONSES_API_TYPE {
        "/responses"
    } else if config.api_type == CHAT_COMPLETIONS_API_TYPE {
        "/chat/completions"
    } else {
        return Err(ApiError::BadRequest("invalid model api type"));
    };
    let bytes = serde_json::to_vec(&body).map_err(|_| ApiError::Internal)?;

    Ok((path.to_string(), bytes))
}

fn model_test_timeout_seconds(config: &ModelConfig) -> u64 {
    if config.config_kind == IMAGE_MODEL_CONFIG_KIND {
        // 图片生成天然比文本补全慢；测试按钮关注连通性，给图片模型一个更实用的下限。
        config.request_timeout_seconds.max(180)
    } else {
        config.request_timeout_seconds
    }
}

fn test_response_message(status: StatusCode, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    if text.is_empty() {
        format!("provider returned HTTP {}", status.as_u16())
    } else {
        text.chars().take(500).collect()
    }
}

fn map_model_test_provider_error(error: LlmProviderError) -> ApiError {
    match error {
        LlmProviderError::InvalidUrl => ApiError::BadGateway("provider url is invalid"),
        LlmProviderError::Timeout => ApiError::GatewayTimeout("provider test timed out"),
        LlmProviderError::LockFailed | LlmProviderError::Failed(_) => {
            ApiError::BadGateway("provider test failed")
        }
    }
}

fn reject_external_instance(instance: &HermesInstance) -> Result<(), ApiError> {
    if instance.kind != HermesInstanceKind::ManagedDocker {
        return Err(ApiError::Conflict(
            "external hermes instance is managed outside hub",
        ));
    }

    Ok(())
}

fn map_provisioner_error(error: ProvisionerError) -> ApiError {
    match error {
        ProvisionerError::InstanceNotFound => ApiError::NotFound("hermes container not found"),
        ProvisionerError::InvalidManagedInstance => {
            ApiError::Conflict("hermes instance is not managed by docker")
        }
        ProvisionerError::LockFailed
        | ProvisionerError::Filesystem(_)
        | ProvisionerError::DockerRuntime(_)
        | ProvisionerError::DockerCommand(_) => ApiError::Internal,
    }
}
