use axum::{
    body::to_bytes,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, time::Instant};
use uuid::Uuid;

use crate::{
    domain::user::{PublicUser, UserListItem, UserRole},
    hermes::{
        docker_provisioner::RuntimeModelSettings,
        instance::{HermesInstance, HermesInstanceKind},
        provisioner::HermesProvisioner,
    },
    http::{
        attachments::{
            drain_multipart_field_with_limit, read_multipart_text_field_with_limit,
            spool_multipart_file_to_temp_with_limit, SpooledMultipartFile,
        },
        auth::require_admin,
        map_provisioner_error, sessions,
        workspace::{
            ensure_home_session_for_hermes_instance, ensure_managed_hermes_for_user,
            ensure_required_model_configs, refresh_managed_hermes_status,
        },
        ApiError,
    },
    llm_proxy::{LlmProviderError, LlmProviderRequest},
    model_config::{
        default_api_type_for_kind, validate_api_type_for_kind, ModelConfig, ModelFallbackConfig,
        CHAT_COMPLETIONS_API_TYPE, DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS,
        DEFAULT_TEMPERATURE, IMAGE_MODEL_CONFIG_KIND, LLM_MODEL_CONFIG_KIND, RESPONSES_API_TYPE,
    },
    public_platform,
    session::store::{
        ApiManagementSettings, BusinessOAuthSettings, HermesSchedulerSnapshot, LdapSettings,
        OidcSettings, PublicPlatformSettings, SpeechInputSettings, StoreError, SystemSettings,
    },
    skills_fs::normalize_skills_path,
    storage::ObjectStorageError,
    AppState,
};

const MAX_MANAGED_SKILL_UPLOAD_FILES: usize = 1000;
const MANAGED_SKILL_DIRECTORY_MARKER: &str = ".hub-directory";
const HERMES_PROFILE_SOUL_FILE: &str = "SOUL.md";
const DEFAULT_PUBLIC_SESSION_PAGE_SIZE: u32 = 10;
const MAX_PUBLIC_SESSION_PAGE_SIZE: u32 = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{user_id}/disable", post(disable_user))
        .route("/api/admin/users/{user_id}/enable", post(enable_user))
        .route("/api/admin/hermes-instances", get(list_hermes_instances))
        .route(
            "/api/admin/public-platform/hermes-instance",
            get(get_public_platform_hermes_instance),
        )
        .route(
            "/api/admin/public-platform/hermes-instance/rebuild",
            post(rebuild_public_platform_hermes_instance),
        )
        .route(
            "/api/admin/public-platform/sessions",
            get(list_public_platform_sessions),
        )
        .route(
            "/api/admin/public-platform/sessions/{session_id}",
            delete(clear_public_platform_session),
        )
        .route(
            "/api/admin/hermes-scheduler-snapshots",
            get(list_hermes_scheduler_snapshots),
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
        .route(
            "/api/admin/model-config/{config_kind}/fallback/test",
            post(test_model_fallback_config),
        )
        .route(
            "/api/admin/system-settings/system",
            put(update_system_parameters),
        )
        .route("/api/admin/system-settings/auth", put(update_auth_settings))
        .route(
            "/api/admin/system-settings/public-platform",
            put(update_public_platform_settings),
        )
        .route(
            "/api/admin/system-settings/api-management",
            put(update_api_management_settings),
        )
        .route(
            "/api/admin/system-settings",
            get(get_system_settings).put(update_system_settings),
        )
        .route(
            "/api/admin/hermes-profile",
            get(get_hermes_profile).put(update_hermes_profile),
        )
        .route("/api/admin/managed-skills", get(list_managed_skills))
        .route(
            "/api/admin/managed-skills/tree",
            get(get_managed_skills_tree),
        )
        .route(
            "/api/admin/managed-skills/upload",
            post(upload_managed_skills).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/admin/managed-skills/directories/{*path}",
            post(create_managed_skill_directory),
        )
        .route(
            "/api/admin/managed-skills/{*path}",
            get(get_managed_skill)
                .put(save_managed_skill)
                .delete(delete_managed_skill),
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
struct PublicPlatformHermesInstanceResponse {
    enabled: bool,
    ready: bool,
    hermes_instance: Option<HermesInstance>,
}

#[derive(Deserialize)]
struct PublicPlatformSessionsQuery {
    page: Option<u32>,
    page_size: Option<u32>,
}

#[derive(Serialize)]
struct PublicPlatformSessionsResponse {
    sessions: Vec<PublicPlatformSessionItem>,
    page: u32,
    page_size: u32,
    total: u64,
    total_pages: u32,
}

#[derive(Serialize)]
struct PublicPlatformSessionItem {
    id: String,
    title: Option<String>,
    created_at: u64,
    updated_at: u64,
    recycle_at: u64,
    public_url: String,
}

impl PublicPlatformSessionsResponse {
    fn empty(page: u32, page_size: u32) -> Self {
        Self {
            sessions: Vec::new(),
            page,
            page_size,
            total: 0,
            total_pages: 0,
        }
    }
}

#[derive(Serialize)]
struct HermesSchedulerSnapshotsResponse {
    hermes_scheduler_snapshots: Vec<HermesSchedulerSnapshot>,
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
    enabled: Option<bool>,
    provider_name: String,
    provider_base_url: String,
    provider_api_key: String,
    default_model: String,
    allowed_models: Option<Vec<String>>,
    api_type: Option<String>,
    reasoning_effort: Option<String>,
    allow_streaming: bool,
    request_timeout_seconds: u64,
    context_window_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    temperature: Option<f64>,
    supports_parallel_tools: Option<bool>,
    fallback: Option<ModelFallbackConfig>,
}

#[derive(Serialize)]
struct ModelConfigTestResponse {
    ok: bool,
    status_code: u16,
    message: String,
    duration_ms: u64,
}

#[derive(Serialize)]
struct SystemSettingsResponse {
    settings: SystemSettings,
}

type UpdateSystemSettingsRequest = SystemSettings;

#[derive(Deserialize)]
struct UpdateSystemParametersRequest {
    max_sessions_per_user: u32,
    max_attachment_upload_bytes: usize,
    attachment_retention_days: u32,
    empty_chat_prompt: String,
    speech_input: SpeechInputSettings,
}

#[derive(Deserialize)]
struct UpdateAuthSettingsRequest {
    oidc: OidcSettings,
    ldap: LdapSettings,
    business_oauth: BusinessOAuthSettings,
}

#[derive(Deserialize)]
struct UpdatePublicPlatformSettingsRequest {
    public_platform: PublicPlatformSettings,
}

#[derive(Deserialize)]
struct UpdateApiManagementSettingsRequest {
    api_management: ApiManagementSettings,
}

#[derive(Clone, Deserialize, Serialize)]
struct HermesProfileContent {
    soul_md: String,
}

#[derive(Serialize)]
struct HermesProfileResponse {
    profile: HermesProfileContent,
}

#[derive(Serialize)]
struct ManagedSkillSummary {
    path: String,
    size: u64,
}

#[derive(Serialize)]
struct ManagedSkillsResponse {
    skills: Vec<ManagedSkillSummary>,
}

#[derive(Clone, Serialize)]
struct ManagedSkillTreeNode {
    name: String,
    path: String,
    kind: &'static str,
    size: u64,
    children: Vec<ManagedSkillTreeNode>,
}

#[derive(Serialize)]
struct ManagedSkillTreeResponse {
    tree: ManagedSkillTreeNode,
}

#[derive(Serialize)]
struct ManagedSkillContent {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct ManagedSkillResponse {
    skill: ManagedSkillContent,
}

#[derive(Deserialize)]
struct SaveManagedSkillRequest {
    content: String,
}

#[derive(Serialize)]
struct ManagedSkillUploadResponse {
    skills: Vec<ManagedSkillSummary>,
}

struct ManagedSkillUploadPart {
    file_name: String,
    file: SpooledMultipartFile,
}

struct ManagedSkillUpload {
    path: String,
    file: SpooledMultipartFile,
}

#[derive(Default)]
struct ManagedSkillTreeBuilder {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    children: BTreeMap<String, ManagedSkillTreeBuilder>,
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
    ensure_not_public_platform_user(&state, &user_id).await?;
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
    ensure_not_public_platform_user(&state, &user_id).await?;
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
    let public_user_id = public_platform::public_user_id(&state).await?;
    let hermes_instances = state
        .store
        .list_hermes_instances()
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut refreshed_instances = Vec::with_capacity(hermes_instances.len());
    for instance in hermes_instances {
        if public_user_id.as_deref() == Some(instance.user_id.as_str()) {
            // 公共 Hermes 是平台级常驻资源，使用独立管理入口，避免混入普通用户实例列表后被误停。
            continue;
        }
        // 管理员列表是运维入口，返回前主动同步 Docker 真实状态，避免 stale running 误导排障。
        refreshed_instances.push(refresh_managed_hermes_status(&state, instance).await?);
    }
    Ok(Json(HermesInstancesResponse {
        hermes_instances: refreshed_instances,
    }))
}

async fn get_public_platform_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    if public_platform::configured_enabled(&state).await? {
        // 管理员打开公共平台配置页时顺手补一次预启动；模型未就绪时只展示 not ready，不阻断页面。
        if let Err(error) = public_platform::ensure_public_hermes_if_enabled(&state).await {
            tracing::warn!(
                ?error,
                "public platform Hermes could not be ensured for admin status"
            );
        }
    }
    let readiness = public_platform::public_hermes_readiness(&state).await?;

    Ok(Json(PublicPlatformHermesInstanceResponse {
        enabled: readiness.configured_enabled,
        ready: readiness.ready,
        hermes_instance: readiness.hermes_instance,
    }))
}

async fn rebuild_public_platform_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    if !public_platform::configured_enabled(&state).await? {
        return Err(ApiError::Conflict("public platform is disabled"));
    }
    let public_user_id = public_platform::ensure_public_owner_user_id(&state).await?;
    let mut instance = match state.store.hermes_instance_for_user(&public_user_id).await {
        Ok(instance) => instance,
        Err(_) => {
            let instance = ensure_managed_hermes_for_user(&state, &public_user_id).await?;
            return Ok(Json(HermesInstanceResponse {
                hermes_instance: instance,
            }));
        }
    };
    ensure_hub_managed_instance(&instance)?;
    // 公共 Hermes 永远不能获得全局 skills 写权限，但必须保留公共 sandbox。
    instance.global_skills_write_enabled = false;
    state
        .docker_provisioner
        .apply_sandbox_policy(&mut instance, true);
    ensure_home_session_for_hermes_instance(&state, &public_user_id, &mut instance).await?;
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
    let image_model = image_config
        .enabled
        .then_some(image_config.default_model.as_str());
    let instance = state
        .docker_provisioner
        .rebuild_instance_with_default_model(
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
    let instance = state
        .store
        .hermes_instance_for_user(&public_user_id)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn list_public_platform_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PublicPlatformSessionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    sessions::cleanup_expired_public_sessions(&state).await?;
    let (page, page_size, offset) = public_sessions_page_params(query);
    let Some(public_user_id) = public_platform::public_user_id(&state).await? else {
        return Ok(Json(PublicPlatformSessionsResponse::empty(page, page_size)));
    };
    let Some(channel) = state
        .channel_store
        .hub_channel_for_user(&public_user_id)
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(Json(PublicPlatformSessionsResponse::empty(page, page_size)));
    };
    let page_result = state
        .channel_store
        .list_sessions_page(&public_user_id, &channel.id, page_size, offset)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut items = Vec::with_capacity(page_result.sessions.len());
    for session in page_result.sessions {
        let recycle_at = sessions::public_session_recycle_at(&state, &session).await?;
        items.push(PublicPlatformSessionItem {
            public_url: format!("/public/sessions/{}", session.id),
            id: session.id,
            title: session.title,
            created_at: session.created_at,
            updated_at: session.updated_at,
            recycle_at,
        });
    }

    Ok(Json(PublicPlatformSessionsResponse {
        sessions: items,
        page,
        page_size,
        total: page_result.total,
        total_pages: total_pages(page_result.total, page_size),
    }))
}

async fn clear_public_platform_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    if Uuid::parse_str(&session_id).is_err() {
        return Err(ApiError::NotFound("public session not found"));
    }
    sessions::force_delete_public_session(&state, &session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn public_sessions_page_params(query: PublicPlatformSessionsQuery) -> (u32, u32, u64) {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(DEFAULT_PUBLIC_SESSION_PAGE_SIZE)
        .clamp(1, MAX_PUBLIC_SESSION_PAGE_SIZE);
    let offset = u64::from(page.saturating_sub(1)).saturating_mul(u64::from(page_size));
    (page, page_size, offset)
}

fn total_pages(total: u64, page_size: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    let page_size = u64::from(page_size.max(1));
    let pages = total.saturating_add(page_size - 1) / page_size;
    u32::try_from(pages).unwrap_or(u32::MAX)
}

async fn list_hermes_scheduler_snapshots(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let public_user_id = public_platform::public_user_id(&state).await?;
    let snapshots = state
        .store
        .list_hermes_scheduler_snapshots()
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .filter(|snapshot| public_user_id.as_deref() != Some(snapshot.user_id.as_str()))
        .collect();

    Ok(Json(HermesSchedulerSnapshotsResponse {
        hermes_scheduler_snapshots: snapshots,
    }))
}

async fn create_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    ensure_not_public_platform_user(&state, &user_id).await?;
    ensure_user_exists(&state, &user_id).await?;

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

async fn ensure_not_public_platform_user(state: &AppState, user_id: &str) -> Result<(), ApiError> {
    if public_platform::is_public_user_id(state, user_id).await? {
        // 公共平台系统用户只允许通过公共 Hermes 专用接口管理，普通用户接口对它保持不可见。
        return Err(ApiError::NotFound("user not found"));
    }

    Ok(())
}

async fn rebuild_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_required_model_configs(&state).await?;
    ensure_not_public_platform_user(&state, &user_id).await?;
    let mut instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    ensure_hub_managed_instance(&instance)?;
    state
        .docker_provisioner
        .apply_sandbox_policy(&mut instance, false);
    // store 中不持久化全局 skills 写权限；管理员重建前必须按用户角色恢复，避免管理员实例被只读挂载。
    instance.global_skills_write_enabled =
        user_has_global_skills_write_access_for_rebuild(&state, &user_id).await?;
    ensure_home_session_for_hermes_instance(&state, &user_id, &mut instance).await?;
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
    let image_model = image_config
        .enabled
        .then_some(image_config.default_model.as_str());
    let instance = state
        .docker_provisioner
        .rebuild_instance_with_default_model(
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
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn user_has_global_skills_write_access_for_rebuild(
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

async fn stop_managed_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_not_public_platform_user(&state, &user_id).await?;
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    ensure_hub_managed_instance(&instance)?;
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
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
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
    ensure_not_public_platform_user(&state, &user_id).await?;
    let instance = state
        .store
        .hermes_instance_for_user(&user_id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;
    ensure_hub_managed_instance(&instance)?;
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
    refresh_managed_hermes_configs(&state).await?;

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
    Ok(Json(execute_model_config_test(&state, &config).await?))
}

async fn test_model_fallback_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(config_kind): Path<String>,
    Json(payload): Json<UpdateModelConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let config = model_config_from_payload(&state, Some(&config_kind), payload).await?;
    let fallback_config = model_config_for_fallback_test(&config)?;
    Ok(Json(
        execute_model_config_test(&state, &fallback_config).await?,
    ))
}

async fn execute_model_config_test(
    state: &AppState,
    config: &ModelConfig,
) -> Result<ModelConfigTestResponse, ApiError> {
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

    Ok(ModelConfigTestResponse {
        ok: status.is_success(),
        status_code: status.as_u16(),
        message,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

async fn get_system_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok((
        [(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, no-store, no-transform"),
        )],
        Json(SystemSettingsResponse { settings }),
    ))
}

async fn update_system_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut payload): Json<UpdateSystemSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    if payload.max_sessions_per_user == 0 {
        return Err(ApiError::BadRequest(
            "max sessions per user must be greater than zero",
        ));
    }
    if payload.max_attachment_upload_bytes == 0 {
        return Err(ApiError::BadRequest(
            "max attachment upload bytes must be greater than zero",
        ));
    }
    if payload.attachment_retention_days == 0 {
        return Err(ApiError::BadRequest(
            "attachment retention days must be greater than zero",
        ));
    }

    // 空会话提示允许不配置；空值会在前端回退到当前语言的默认文案。
    payload.empty_chat_prompt = payload.empty_chat_prompt.trim().to_string();
    let should_ensure_public_hermes = payload.public_platform.enabled;
    persist_system_settings_update(&state, move |settings| {
        *settings = payload;
    })
    .await?;
    post_system_settings_save(&state, should_ensure_public_hermes).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn update_system_parameters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut payload): Json<UpdateSystemParametersRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    if payload.max_sessions_per_user == 0 {
        return Err(ApiError::BadRequest(
            "max sessions per user must be greater than zero",
        ));
    }
    if payload.max_attachment_upload_bytes == 0 {
        return Err(ApiError::BadRequest(
            "max attachment upload bytes must be greater than zero",
        ));
    }
    if payload.attachment_retention_days == 0 {
        return Err(ApiError::BadRequest(
            "attachment retention days must be greater than zero",
        ));
    }

    payload.empty_chat_prompt = payload.empty_chat_prompt.trim().to_string();
    state
        .store
        .update_system_parameter_settings(
            payload.max_sessions_per_user,
            payload.max_attachment_upload_bytes,
            payload.attachment_retention_days,
            payload.empty_chat_prompt,
            payload.speech_input,
        )
        .await
        .map_err(map_system_settings_error)?;
    post_system_settings_save(&state, false).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn update_auth_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateAuthSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    state
        .store
        .update_auth_settings(payload.oidc, payload.ldap, payload.business_oauth)
        .await
        .map_err(map_system_settings_error)?;
    post_system_settings_save(&state, false).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn update_public_platform_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdatePublicPlatformSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let should_ensure_public_hermes = payload.public_platform.enabled;
    state
        .store
        .update_public_platform_settings(payload.public_platform)
        .await
        .map_err(map_system_settings_error)?;
    post_system_settings_save(&state, should_ensure_public_hermes).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn update_api_management_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateApiManagementSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    state
        .store
        .update_api_management_settings(payload.api_management)
        .await
        .map_err(map_system_settings_error)?;
    post_system_settings_save(&state, false).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn persist_system_settings_update<F>(
    state: &AppState,
    mutate: F,
) -> Result<SystemSettings, ApiError>
where
    F: FnOnce(&mut SystemSettings),
{
    let mut settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    mutate(&mut settings);
    state
        .store
        .update_system_settings(settings)
        .await
        .map_err(|_| ApiError::BadRequest("invalid system settings"))
}

async fn post_system_settings_save(
    state: &AppState,
    should_ensure_public_hermes: bool,
) -> Result<(), ApiError> {
    refresh_managed_hermes_configs(state).await?;
    if should_ensure_public_hermes {
        if let Err(error) = public_platform::ensure_public_hermes_if_enabled(state).await {
            tracing::warn!(
                ?error,
                "public platform Hermes could not be ensured after settings save"
            );
        }
    }
    Ok(())
}

fn map_system_settings_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InvalidSystemSettings => ApiError::BadRequest("invalid system settings"),
        _ => ApiError::Internal,
    }
}

async fn get_hermes_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let profile = HermesProfileContent {
        soul_md: read_hermes_profile_file(&state, HERMES_PROFILE_SOUL_FILE).await?,
    };

    Ok(Json(HermesProfileResponse { profile }))
}

async fn update_hermes_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<HermesProfileContent>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    write_hermes_profile_file(&state, HERMES_PROFILE_SOUL_FILE, payload.soul_md.as_bytes()).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn read_hermes_profile_file(state: &AppState, file_name: &str) -> Result<String, ApiError> {
    let key = hermes_profile_object_key(state, file_name)?;
    match state.object_storage.get(&key).await {
        Ok(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|_| ApiError::BadGateway("hermes profile file is not valid utf-8")),
        Err(ObjectStorageError::NotFound) => Ok(String::new()),
        Err(error) => Err(map_object_storage_error(error)),
    }
}

async fn write_hermes_profile_file(
    state: &AppState,
    file_name: &str,
    bytes: &[u8],
) -> Result<(), ApiError> {
    let key = hermes_profile_object_key(state, file_name)?;
    state
        .object_storage
        .put(&key, Bytes::copy_from_slice(bytes))
        .await
        .map_err(map_object_storage_error)
}

async fn refresh_managed_hermes_configs(state: &AppState) -> Result<(), ApiError> {
    let instances = state
        .store
        .list_hermes_instances()
        .await
        .map_err(|_| ApiError::Internal)?;
    if instances.is_empty() {
        return Ok(());
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
    let model_settings = runtime_model_settings(&llm_config);
    let public_user_id = public_platform::public_user_id(state).await?;
    let public_platform_enabled = public_platform::configured_enabled(state).await?;

    for mut instance in instances {
        if instance.kind != HermesInstanceKind::ManagedDocker {
            continue;
        }
        if !public_platform_enabled && public_user_id.as_deref() == Some(instance.user_id.as_str())
        {
            // 公共平台关闭时，保存模型/系统配置不能间接重建隐藏公共 Hermes。
            continue;
        }
        let original_instance = instance.clone();
        let sandbox_enabled = public_platform::is_public_user_id(state, &instance.user_id).await?;
        state
            .docker_provisioner
            .apply_sandbox_policy(&mut instance, sandbox_enabled);
        let instance_user_id = instance.user_id.clone();
        ensure_home_session_for_hermes_instance(state, &instance_user_id, &mut instance).await?;
        let path_policy_changed = instance.host_workspace_path
            != original_instance.host_workspace_path
            || instance.host_config_path != original_instance.host_config_path
            || instance.host_sandbox_path != original_instance.host_sandbox_path;
        let llm_api_key = match instance.api_token_secret_ref.clone() {
            Some(existing_token) => {
                state
                    .model_registry
                    .add_instance_token_for_instance(&instance.id, &existing_token)
                    .await
                    .map_err(|_| ApiError::Internal)?;
                existing_token
            }
            None => {
                let token = state
                    .model_registry
                    .issue_instance_token_for_instance(&instance.id)
                    .await
                    .map_err(|_| ApiError::Internal)?;
                instance.api_token_secret_ref = Some(token.clone());
                token
            }
        };
        if path_policy_changed {
            // 只更新 config 无法移除旧容器里已经存在的 bind mount；路径策略变化必须立即重建容器。
            let rebuilt = state
                .docker_provisioner
                .rebuild_instance_with_default_model(
                    &instance,
                    &llm_api_key,
                    &model_settings,
                    image_model,
                )
                .await
                .map_err(map_provisioner_error)?;
            state
                .store
                .bind_hermes_instance(rebuilt)
                .await
                .map_err(|_| ApiError::Internal)?;
            continue;
        }
        if instance.api_token_secret_ref != original_instance.api_token_secret_ref {
            state
                .store
                .bind_hermes_instance(instance.clone())
                .await
                .map_err(|_| ApiError::Internal)?;
        }
        let changed = state
            .docker_provisioner
            .write_config_with_default_model(&instance, &llm_api_key, &model_settings, image_model)
            .await
            .map_err(map_provisioner_error)?;
        if changed {
            state
                .store
                .request_hermes_gateway_restart(&instance.id)
                .await
                .map_err(|_| ApiError::Internal)?;
        }
    }

    Ok(())
}

async fn list_managed_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let skills = list_managed_skill_summaries(&state).await?;

    Ok(Json(ManagedSkillsResponse { skills }))
}

async fn get_managed_skills_tree(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let prefix = managed_skills_prefix(&state)?;
    let list_prefix = managed_skills_list_prefix(&prefix);
    let mut root = ManagedSkillTreeBuilder::root();

    for object in state
        .object_storage
        .list_prefix(&list_prefix)
        .await
        .map_err(map_object_storage_error)?
    {
        if let Some(path) = managed_skill_directory_marker_path(&prefix, &object.key) {
            root.insert_dir(&path)?;
            continue;
        }
        if let Some(path) = managed_skill_relative_path(&prefix, &object.key) {
            root.insert_file(&path, object.size)?;
        }
    }

    Ok(Json(ManagedSkillTreeResponse {
        tree: root.into_node(),
    }))
}

async fn get_managed_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let path = normalize_managed_skill_path(&path)?;
    let key = managed_skill_object_key(&managed_skills_prefix(&state)?, &path);
    let bytes = state
        .object_storage
        .get(&key)
        .await
        .map_err(map_object_storage_error)?;
    let content = String::from_utf8(bytes.to_vec())
        .map_err(|_| ApiError::BadGateway("managed skill is not valid utf-8"))?;

    Ok(Json(ManagedSkillResponse {
        skill: ManagedSkillContent { path, content },
    }))
}

async fn save_managed_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
    Json(payload): Json<SaveManagedSkillRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let path = normalize_managed_skill_path(&path)?;
    let key = managed_skill_object_key(&managed_skills_prefix(&state)?, &path);
    state
        .object_storage
        .put(&key, payload.content.clone().into())
        .await
        .map_err(map_object_storage_error)?;

    Ok(Json(ManagedSkillResponse {
        skill: ManagedSkillContent {
            path,
            content: payload.content,
        },
    }))
}

async fn delete_managed_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let path = normalize_managed_skill_path(&path)?;
    let prefix = managed_skills_prefix(&state)?;
    let key = managed_skill_object_key(&prefix, &path);
    let list_prefix = managed_skill_object_key(&prefix, &format!("{path}/"));
    let mut keys = state
        .object_storage
        .list_prefix(&list_prefix)
        .await
        .map_err(map_object_storage_error)?
        .into_iter()
        .map(|object| object.key)
        .collect::<Vec<_>>();

    match state.object_storage.get(&key).await {
        Ok(_) => keys.push(key),
        Err(ObjectStorageError::NotFound) => {}
        Err(error) => return Err(map_object_storage_error(error)),
    }

    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        return Err(ApiError::NotFound("managed skill not found"));
    }

    for key in keys {
        state
            .object_storage
            .delete(&key)
            .await
            .map_err(map_object_storage_error)?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn upload_managed_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let uploads = parse_managed_skill_upload(&state, multipart).await?;
    if uploads.is_empty() {
        return Err(ApiError::BadRequest("managed skill upload is empty"));
    }
    let prefix = managed_skills_prefix(&state)?;
    let mut skills = Vec::with_capacity(uploads.len());

    for mut upload in uploads {
        let path = upload.path;
        let key = managed_skill_object_key(&prefix, &path);
        let size = upload.file.size;
        let upload_result = state
            .object_storage
            .put_file(&key, upload.file.path())
            .await;
        upload.file.cleanup().await;
        upload_result.map_err(map_object_storage_error)?;
        skills.push(ManagedSkillSummary { path, size });
    }
    skills.sort_by(|left, right| left.path.cmp(&right.path));

    Ok((
        StatusCode::CREATED,
        Json(ManagedSkillUploadResponse { skills }),
    ))
}

async fn create_managed_skill_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let path = normalize_managed_skill_path(&path)?;
    let marker_path = format!("{path}/{MANAGED_SKILL_DIRECTORY_MARKER}");
    let key = managed_skill_object_key(&managed_skills_prefix(&state)?, &marker_path);
    state
        .object_storage
        .put(&key, Bytes::new())
        .await
        .map_err(map_object_storage_error)?;

    Ok(StatusCode::CREATED)
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
    let existing_config = state
        .model_registry
        .config_for_kind(&config_kind)
        .await
        .ok();
    let provider_api_key = if payload.provider_api_key.trim().is_empty() {
        // 空字符串表示沿用已保存密钥，方便管理员只改模型名或 Base URL。
        existing_config
            .as_ref()
            .ok_or(ApiError::BadRequest("invalid model config kind"))?
            .provider_api_key
            .clone()
    } else {
        payload.provider_api_key
    };
    let enabled = match payload.enabled {
        Some(enabled) => enabled,
        None => existing_config
            .as_ref()
            .map(|config| config.enabled)
            .unwrap_or(config_kind != IMAGE_MODEL_CONFIG_KIND),
    };

    let fallback =
        fallback_config_from_payload(&config_kind, payload.fallback, existing_config.as_ref());

    Ok(ModelConfig {
        api_type: payload
            .api_type
            .unwrap_or_else(|| default_api_type_for_kind(&config_kind).to_string()),
        reasoning_effort: payload.reasoning_effort,
        config_kind,
        enabled,
        provider_name: payload.provider_name,
        provider_base_url: payload.provider_base_url,
        provider_api_key,
        default_model: payload.default_model,
        allowed_models: payload.allowed_models.unwrap_or_default(),
        allow_streaming: payload.allow_streaming,
        request_timeout_seconds: payload.request_timeout_seconds,
        context_window_tokens: payload
            .context_window_tokens
            .or_else(|| {
                existing_config
                    .as_ref()
                    .map(|config| config.context_window_tokens)
            })
            .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS),
        max_output_tokens: payload
            .max_output_tokens
            .or_else(|| {
                existing_config
                    .as_ref()
                    .map(|config| config.max_output_tokens)
            })
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        temperature: payload
            .temperature
            .or_else(|| existing_config.as_ref().map(|config| config.temperature))
            .unwrap_or(DEFAULT_TEMPERATURE),
        supports_parallel_tools: payload
            .supports_parallel_tools
            .or_else(|| {
                existing_config
                    .as_ref()
                    .map(|config| config.supports_parallel_tools)
            })
            .unwrap_or(true),
        fallback,
    })
}

fn fallback_config_from_payload(
    config_kind: &str,
    fallback: Option<ModelFallbackConfig>,
    existing_config: Option<&ModelConfig>,
) -> Option<ModelFallbackConfig> {
    if config_kind == IMAGE_MODEL_CONFIG_KIND {
        return None;
    }
    let Some(mut fallback) = fallback else {
        return existing_config.and_then(|config| config.fallback.clone());
    };
    if fallback.provider_api_key.trim().is_empty() {
        // fallback API key 和主 API key 一样，空字符串表示沿用后端已保存值。
        if let Some(existing_key) = existing_config
            .and_then(|config| config.fallback.as_ref())
            .map(|fallback| fallback.provider_api_key.clone())
        {
            fallback.provider_api_key = existing_key;
        }
    }
    Some(fallback)
}

fn model_config_for_fallback_test(config: &ModelConfig) -> Result<ModelConfig, ApiError> {
    if config.config_kind == IMAGE_MODEL_CONFIG_KIND {
        return Err(ApiError::BadRequest(
            "fallback model test is not available for image model",
        ));
    }
    let fallback = config
        .fallback
        .as_ref()
        .filter(|fallback| fallback.enabled)
        .ok_or(ApiError::BadRequest("fallback model is not enabled"))?;
    if fallback.provider_name.trim().is_empty()
        || fallback.provider_base_url.trim().is_empty()
        || fallback.provider_api_key.trim().is_empty()
        || fallback.default_model.trim().is_empty()
    {
        return Err(ApiError::BadRequest("fallback model config is incomplete"));
    }

    // fallback 单独测试复用主模型测试报文，但所有 provider/model/运行时参数都切到 fallback。
    let mut fallback_config = config.clone();
    fallback_config.provider_name = fallback.provider_name.clone();
    fallback_config.provider_base_url = fallback.provider_base_url.clone();
    fallback_config.provider_api_key = fallback.provider_api_key.clone();
    fallback_config.default_model = fallback.default_model.clone();
    fallback_config.allowed_models = fallback.allowed_models.clone();
    fallback_config.api_type = fallback.api_type.clone();
    fallback_config.reasoning_effort = fallback.reasoning_effort.clone();
    fallback_config.allow_streaming = fallback.allow_streaming;
    fallback_config.request_timeout_seconds = fallback.request_timeout_seconds;
    fallback_config.context_window_tokens = fallback.context_window_tokens;
    fallback_config.max_output_tokens = fallback.max_output_tokens;
    fallback_config.temperature = fallback.temperature;
    fallback_config.supports_parallel_tools = fallback.supports_parallel_tools;
    fallback_config.fallback = None;
    Ok(fallback_config)
}

fn runtime_model_settings(config: &ModelConfig) -> RuntimeModelSettings {
    RuntimeModelSettings {
        default_model: config.default_model.clone(),
        api_mode: config.api_type.clone(),
        context_window_tokens: config.context_window_tokens,
        max_output_tokens: config.max_output_tokens,
        temperature: config.temperature,
        supports_parallel_tools: config.supports_parallel_tools,
    }
}

fn managed_skills_prefix(state: &AppState) -> Result<String, ApiError> {
    let prefix = state.config.skills_fs.prefix.trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    normalize_skills_path(prefix).ok_or(ApiError::Internal)
}

fn hermes_profile_object_key(state: &AppState, file_name: &str) -> Result<String, ApiError> {
    let prefix = state.config.managed_profile.prefix.trim_matches('/');
    let file_name = normalize_skills_path(file_name).ok_or(ApiError::Internal)?;
    if prefix.is_empty() {
        return Ok(file_name);
    }
    let prefix = normalize_skills_path(prefix).ok_or(ApiError::Internal)?;
    Ok(format!("{prefix}/{file_name}"))
}

fn managed_skills_list_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    }
}

async fn list_managed_skill_summaries(
    state: &AppState,
) -> Result<Vec<ManagedSkillSummary>, ApiError> {
    let prefix = managed_skills_prefix(state)?;
    let list_prefix = managed_skills_list_prefix(&prefix);
    let mut skills = state
        .object_storage
        .list_prefix(&list_prefix)
        .await
        .map_err(map_object_storage_error)?
        .into_iter()
        .filter_map(|object| {
            managed_skill_relative_path(&prefix, &object.key).map(|path| ManagedSkillSummary {
                path,
                size: object.size,
            })
        })
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(skills)
}

fn managed_skill_object_key(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{prefix}/{path}")
    }
}

fn managed_skill_relative_path(prefix: &str, key: &str) -> Option<String> {
    let key = key.trim_start_matches('/');
    if key.is_empty() || key.ends_with('/') {
        return None;
    }
    let relative = if prefix.is_empty() {
        key
    } else {
        key.strip_prefix(&format!("{prefix}/"))?
    };
    if relative
        .rsplit('/')
        .next()
        .is_some_and(|name| name == MANAGED_SKILL_DIRECTORY_MARKER)
    {
        return None;
    }
    normalize_managed_skill_path(relative).ok()
}

fn managed_skill_directory_marker_path(prefix: &str, key: &str) -> Option<String> {
    let key = key.trim_start_matches('/');
    let relative = if prefix.is_empty() {
        key
    } else {
        key.strip_prefix(&format!("{prefix}/"))?
    };
    let directory = relative.strip_suffix(&format!("/{MANAGED_SKILL_DIRECTORY_MARKER}"))?;
    normalize_managed_skill_path(directory).ok()
}

async fn parse_managed_skill_upload(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<Vec<ManagedSkillUpload>, ApiError> {
    let mut target_path = String::new();
    let mut parts = Vec::new();
    let mut total_bytes = 0usize;
    let max_upload_bytes = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?
        .max_attachment_upload_bytes;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        let field_name = field.name().map(ToOwned::to_owned).unwrap_or_default();
        if field_name == "target_path" {
            let value = read_multipart_text_field_with_limit(
                field,
                4096,
                "managed skill target path is too large",
            )
            .await?;
            target_path = normalize_managed_skill_optional_path(&value)?;
            continue;
        }

        let Some(file_name) = field.file_name().map(ToOwned::to_owned) else {
            drain_multipart_field_with_limit(field, 64 * 1024, "multipart field is too large")
                .await?;
            continue;
        };
        let content_type = field.content_type().map(ToOwned::to_owned);
        if managed_skill_upload_is_zip(&file_name, content_type.as_deref()) {
            // 统一 Skill 管理只接收展开后的普通文件，压缩包上传统一在解析阶段拒绝。
            return Err(ApiError::BadRequest(
                "managed skill zip uploads are not supported",
            ));
        }
        let remaining_upload_bytes = max_upload_bytes.saturating_sub(total_bytes);
        let file =
            spool_multipart_file_to_temp_with_limit(field, Some(remaining_upload_bytes)).await?;
        let file_size = usize::try_from(file.size)
            .map_err(|_| ApiError::BadRequest("managed skill upload is too large"))?;
        total_bytes = total_bytes
            .checked_add(file_size)
            .ok_or(ApiError::BadRequest("managed skill upload is too large"))?;
        if total_bytes > max_upload_bytes {
            return Err(ApiError::BadRequest("managed skill upload is too large"));
        }
        parts.push(ManagedSkillUploadPart { file_name, file });
        if parts.len() > MAX_MANAGED_SKILL_UPLOAD_FILES {
            return Err(ApiError::BadRequest(
                "managed skill upload has too many files",
            ));
        }
    }

    let mut uploads = Vec::new();
    for part in parts {
        let path = normalize_managed_skill_upload_path(&target_path, &part.file_name)?;
        uploads.push(ManagedSkillUpload {
            path,
            file: part.file,
        });
        if uploads.len() > MAX_MANAGED_SKILL_UPLOAD_FILES {
            return Err(ApiError::BadRequest(
                "managed skill upload has too many files",
            ));
        }
    }

    uploads.sort_by(|left, right| left.path.cmp(&right.path));
    uploads.dedup_by(|left, right| left.path == right.path);
    Ok(uploads)
}

fn managed_skill_upload_is_zip(file_name: &str, content_type: Option<&str>) -> bool {
    file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("zip"))
        || content_type.is_some_and(|content_type| {
            matches!(
                content_type,
                "application/zip" | "application/x-zip-compressed"
            )
        })
}

fn normalize_managed_skill_optional_path(path: &str) -> Result<String, ApiError> {
    let path = path.trim().trim_matches('/');
    if path.is_empty() {
        return Ok(String::new());
    }
    normalize_managed_skill_path(path)
}

fn normalize_managed_skill_upload_path(target_path: &str, path: &str) -> Result<String, ApiError> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Err(ApiError::BadRequest("invalid managed skill path"));
    }
    let combined = if target_path.is_empty() {
        path.to_string()
    } else {
        format!("{target_path}/{path}")
    };
    normalize_managed_skill_path(&combined)
}

fn normalize_managed_skill_path(path: &str) -> Result<String, ApiError> {
    let path = path.trim_start_matches('/');
    if path.ends_with('/') {
        return Err(ApiError::BadRequest("invalid managed skill path"));
    }
    let normalized =
        normalize_skills_path(path).ok_or(ApiError::BadRequest("invalid managed skill path"))?;
    if normalized.is_empty() || has_hidden_managed_skill_segment(&normalized) {
        return Err(ApiError::BadRequest("invalid managed skill path"));
    }
    Ok(normalized)
}

impl ManagedSkillTreeBuilder {
    fn root() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            is_dir: true,
            size: 0,
            children: BTreeMap::new(),
        }
    }

    fn insert_file(&mut self, path: &str, size: u64) -> Result<(), ApiError> {
        let path = normalize_managed_skill_path(path)?;
        let mut current = self;
        let mut current_path = String::new();
        let mut segments = path.split('/').peekable();

        while let Some(segment) = segments.next() {
            let is_file = segments.peek().is_none();
            current_path = if current_path.is_empty() {
                segment.to_string()
            } else {
                format!("{current_path}/{segment}")
            };
            current = current
                .children
                .entry(segment.to_string())
                .or_insert_with(|| ManagedSkillTreeBuilder {
                    name: segment.to_string(),
                    path: current_path.clone(),
                    is_dir: !is_file,
                    size: 0,
                    children: BTreeMap::new(),
                });
            if is_file {
                current.is_dir = false;
                current.size = size;
            }
        }

        Ok(())
    }

    fn insert_dir(&mut self, path: &str) -> Result<(), ApiError> {
        let path = normalize_managed_skill_path(path)?;
        let mut current = self;
        let mut current_path = String::new();

        for segment in path.split('/') {
            current_path = if current_path.is_empty() {
                segment.to_string()
            } else {
                format!("{current_path}/{segment}")
            };
            current = current
                .children
                .entry(segment.to_string())
                .or_insert_with(|| ManagedSkillTreeBuilder {
                    name: segment.to_string(),
                    path: current_path.clone(),
                    is_dir: true,
                    size: 0,
                    children: BTreeMap::new(),
                });
            current.is_dir = true;
        }

        Ok(())
    }

    fn into_node(self) -> ManagedSkillTreeNode {
        let mut children = self
            .children
            .into_values()
            .map(ManagedSkillTreeBuilder::into_node)
            .collect::<Vec<_>>();
        // 文件夹排在文件前面，同类按路径稳定排序，前端刷新时树不会跳动。
        children.sort_by(|left, right| match (left.kind, right.kind) {
            ("dir", "file") => std::cmp::Ordering::Less,
            ("file", "dir") => std::cmp::Ordering::Greater,
            _ => left.name.cmp(&right.name),
        });
        ManagedSkillTreeNode {
            name: self.name,
            path: self.path,
            kind: if self.is_dir { "dir" } else { "file" },
            size: self.size,
            children,
        }
    }
}

fn has_hidden_managed_skill_segment(path: &str) -> bool {
    // 统一管理的 Skill 不能写入任何隐藏文件或隐藏目录；内部目录 marker 会先 strip suffix 再进入这里。
    path.split('/').any(|segment| segment.starts_with('.'))
}

fn map_object_storage_error(error: ObjectStorageError) -> ApiError {
    match error {
        ObjectStorageError::NotFound => ApiError::NotFound("managed skill not found"),
        ObjectStorageError::NotConfigured => ApiError::Internal,
        ObjectStorageError::LockFailed | ObjectStorageError::OperationFailed => {
            ApiError::BadGateway("object storage operation failed")
        }
    }
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

fn ensure_hub_managed_instance(instance: &HermesInstance) -> Result<(), ApiError> {
    if instance.kind != HermesInstanceKind::ManagedDocker {
        return Err(ApiError::Conflict("hermes runtime must be managed by hub"));
    }

    Ok(())
}
