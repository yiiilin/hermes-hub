use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use getrandom::fill as getrandom_fill;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::security::crypto::{decrypt_secret, encrypt_secret, SecretCipher};

pub const LLM_MODEL_CONFIG_KIND: &str = "llm";
pub const IMAGE_MODEL_CONFIG_KIND: &str = "image";
pub const TITLE_MODEL_CONFIG_KIND: &str = "title";
pub const MODEL_CONFIG_KINDS: [&str; 3] = [
    LLM_MODEL_CONFIG_KIND,
    IMAGE_MODEL_CONFIG_KIND,
    TITLE_MODEL_CONFIG_KIND,
];
pub const REQUIRED_RUNTIME_MODEL_CONFIG_KINDS: [&str; 2] =
    [LLM_MODEL_CONFIG_KIND, TITLE_MODEL_CONFIG_KIND];
pub const CHAT_COMPLETIONS_API_TYPE: &str = "chat_completions";
pub const RESPONSES_API_TYPE: &str = "responses";
pub const IMAGES_GENERATIONS_API_TYPE: &str = "images_generations";
pub const REASONING_EFFORTS: [&str; 4] = ["minimal", "low", "medium", "high"];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelConfig {
    pub config_kind: String,
    pub provider_name: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub api_type: String,
    pub reasoning_effort: Option<String>,
    pub allow_streaming: bool,
    pub request_timeout_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequiredModelConfigReadiness {
    pub ready: bool,
    pub missing_config_kinds: Vec<String>,
}

/// LLM 配置和 Hermes instance token 注册表。
///
/// 内存后端用于测试；Postgres 后端负责持久化 model config 和 token hash。
#[derive(Clone)]
pub struct ModelRegistry {
    backend: ModelRegistryBackend,
}

#[derive(Clone)]
enum ModelRegistryBackend {
    Memory(Arc<Mutex<ModelRegistryInner>>),
    Postgres(PostgresModelRegistry),
}

#[derive(Clone)]
struct PostgresModelRegistry {
    pool: PgPool,
    cipher: SecretCipher,
}

#[derive(Clone)]
struct ModelRegistryInner {
    configs_by_kind: HashMap<String, ModelConfig>,
    instance_tokens_by_hash: HashMap<String, Option<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceTokenContext {
    pub hermes_instance_id: Option<String>,
    pub user_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedModelRequest {
    pub config: ModelConfig,
    pub body: Vec<u8>,
    pub model: String,
}

#[derive(Debug, Error)]
pub enum ModelRegistryError {
    #[error("model registry lock failed")]
    LockFailed,
    #[error("model is not allowed")]
    ModelNotAllowed,
    #[error("invalid model request")]
    InvalidRequest,
    #[error("streaming is disabled")]
    StreamingDisabled,
    #[error("database operation failed")]
    DatabaseFailed,
    #[error("secret operation failed")]
    SecretFailed,
}

impl ModelRegistry {
    pub fn new(config: ModelConfig) -> Self {
        let configs_by_kind = default_config_set(config);
        Self {
            backend: ModelRegistryBackend::Memory(Arc::new(Mutex::new(ModelRegistryInner {
                configs_by_kind,
                instance_tokens_by_hash: HashMap::new(),
            }))),
        }
    }

    pub async fn postgres(
        pool: PgPool,
        cipher: SecretCipher,
        default_config: ModelConfig,
    ) -> Result<Self, ModelRegistryError> {
        let registry = Self {
            backend: ModelRegistryBackend::Postgres(PostgresModelRegistry { pool, cipher }),
        };
        registry.ensure_postgres_config(default_config).await?;
        Ok(registry)
    }

    pub fn default_for_tests() -> Self {
        Self::new(ModelConfig {
            config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
            provider_name: "openai-compatible".to_string(),
            provider_base_url: "https://provider.example/v1".to_string(),
            provider_api_key: "provider-secret".to_string(),
            default_model: "gpt-4.1-mini".to_string(),
            allowed_models: vec!["gpt-4.1-mini".to_string()],
            api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
            reasoning_effort: None,
            allow_streaming: true,
            request_timeout_seconds: 60,
        })
    }

    pub fn add_instance_token(&self, token: &str) {
        if let ModelRegistryBackend::Memory(inner) = &self.backend {
            if let Ok(mut inner) = inner.lock() {
                inner
                    .instance_tokens_by_hash
                    .insert(hash_token(token), None);
            }
        }
    }

    pub async fn add_instance_token_for_instance(
        &self,
        instance_id: &str,
        token: &str,
    ) -> Result<(), ModelRegistryError> {
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                inner
                    .instance_tokens_by_hash
                    .insert(hash_token(token), Some(instance_id.to_string()));
                Ok(())
            }
            ModelRegistryBackend::Postgres(store) => {
                store
                    .add_instance_token_for_instance(instance_id, token)
                    .await
            }
        }
    }

    pub async fn revoke_instance_tokens_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<(), ModelRegistryError> {
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                inner
                    .instance_tokens_by_hash
                    .retain(|_, stored_instance_id| {
                        stored_instance_id.as_deref() != Some(instance_id)
                    });
                Ok(())
            }
            ModelRegistryBackend::Postgres(store) => {
                store.revoke_instance_tokens_for_instance(instance_id).await
            }
        }
    }

    /// 生成新的 Hermes 实例访问令牌，并持久化对应 hash。
    pub async fn issue_instance_token_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<String, ModelRegistryError> {
        let token = random_token()?;
        self.add_instance_token_for_instance(instance_id, &token)
            .await?;
        Ok(token)
    }

    /// 兼容内存测试场景的令牌生成入口。
    pub fn issue_instance_token(&self) -> Result<String, ModelRegistryError> {
        let token = random_token()?;
        self.add_instance_token(&token);
        Ok(token)
    }

    pub async fn verify_instance_token(&self, token: &str) -> bool {
        self.instance_token_context(token).await.is_some()
    }

    pub async fn instance_token_context(&self, token: &str) -> Option<InstanceTokenContext> {
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => inner
                .lock()
                .ok()
                .and_then(|inner| {
                    inner
                        .instance_tokens_by_hash
                        .get(&hash_token(token))
                        .cloned()
                })
                .map(|instance_id| InstanceTokenContext {
                    hermes_instance_id: instance_id,
                    user_id: None,
                }),
            ModelRegistryBackend::Postgres(store) => store.instance_token_context(token).await,
        }
    }

    pub async fn active_config(&self) -> Result<ModelConfig, ModelRegistryError> {
        self.config_for_kind(LLM_MODEL_CONFIG_KIND).await
    }

    pub async fn config_for_kind(&self, kind: &str) -> Result<ModelConfig, ModelRegistryError> {
        validate_config_kind(kind)?;
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                inner
                    .configs_by_kind
                    .get(kind)
                    .cloned()
                    .ok_or(ModelRegistryError::InvalidRequest)
            }
            ModelRegistryBackend::Postgres(store) => store.active_config(kind).await,
        }
    }

    pub async fn all_configs(&self) -> Result<Vec<ModelConfig>, ModelRegistryError> {
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                MODEL_CONFIG_KINDS
                    .iter()
                    .map(|kind| {
                        inner
                            .configs_by_kind
                            .get(*kind)
                            .cloned()
                            .ok_or(ModelRegistryError::InvalidRequest)
                    })
                    .collect()
            }
            ModelRegistryBackend::Postgres(store) => {
                let mut configs = Vec::with_capacity(MODEL_CONFIG_KINDS.len());
                for kind in MODEL_CONFIG_KINDS {
                    configs.push(store.active_config(kind).await?);
                }
                Ok(configs)
            }
        }
    }

    pub async fn required_runtime_config_readiness(
        &self,
    ) -> Result<RequiredModelConfigReadiness, ModelRegistryError> {
        let mut missing_config_kinds = Vec::new();

        for kind in REQUIRED_RUNTIME_MODEL_CONFIG_KINDS {
            let config = self.config_for_kind(kind).await?;
            if !is_runtime_model_config_ready(&config) {
                missing_config_kinds.push(kind.to_string());
            }
        }

        Ok(RequiredModelConfigReadiness {
            ready: missing_config_kinds.is_empty(),
            missing_config_kinds,
        })
    }

    pub async fn replace(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        validate_config_kind(&config.config_kind)?;
        let config = normalize_model_config(config)?;
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                inner
                    .configs_by_kind
                    .insert(config.config_kind.clone(), config);
                Ok(())
            }
            ModelRegistryBackend::Postgres(store) => store.replace(config).await,
        }
    }

    pub async fn models_payload(&self) -> Result<Value, ModelRegistryError> {
        let config = self.active_config().await?;
        let models = config
            .allowed_models
            .iter()
            .map(|model| json!({ "id": model, "object": "model" }))
            .collect::<Vec<_>>();

        Ok(json!({
            "object": "list",
            "data": models
        }))
    }

    pub async fn prepare_request_body(
        &self,
        body: Value,
        api_type: &str,
    ) -> Result<PreparedModelRequest, ModelRegistryError> {
        self.prepare_request_body_for_kind(body, LLM_MODEL_CONFIG_KIND, api_type)
            .await
    }

    pub async fn prepare_request_body_for_kind(
        &self,
        mut body: Value,
        config_kind: &str,
        api_type: &str,
    ) -> Result<PreparedModelRequest, ModelRegistryError> {
        let config = self.config_for_kind(config_kind).await?;
        validate_api_type_for_kind(config_kind, api_type)?;
        let object = body
            .as_object_mut()
            .ok_or(ModelRegistryError::InvalidRequest)?;
        let requested = object
            .get("model")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let model = match requested {
            Some(model)
                if config
                    .allowed_models
                    .iter()
                    .any(|allowed| allowed == &model) =>
            {
                model
            }
            Some(_) if config_kind == IMAGE_MODEL_CONFIG_KIND => config.default_model.clone(),
            Some(_) => return Err(ModelRegistryError::ModelNotAllowed),
            None => config.default_model.clone(),
        };

        if !config
            .allowed_models
            .iter()
            .any(|allowed| allowed == &model)
        {
            return Err(ModelRegistryError::ModelNotAllowed);
        }

        object.insert("model".to_string(), Value::String(model.clone()));
        if config_kind != IMAGE_MODEL_CONFIG_KIND {
            apply_reasoning_config(object, &config, api_type);
        } else {
            sanitize_image_generation_request(object);
        }
        if object
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            && !config.allow_streaming
        {
            return Err(ModelRegistryError::StreamingDisabled);
        }

        let bytes = serde_json::to_vec(&Value::Object(object.clone()))
            .map_err(|_| ModelRegistryError::InvalidRequest)?;

        Ok(PreparedModelRequest {
            config,
            body: bytes,
            model,
        })
    }

    async fn ensure_postgres_config(
        &self,
        default_config: ModelConfig,
    ) -> Result<(), ModelRegistryError> {
        let ModelRegistryBackend::Postgres(store) = &self.backend else {
            return Ok(());
        };

        for config in default_config_set(default_config).into_values() {
            let active: Option<(Uuid,)> = sqlx::query_as(
                "select id from model_configs where is_active = true and config_kind = $1 limit 1",
            )
            .bind(&config.config_kind)
            .fetch_optional(&store.pool)
            .await
            .map_err(|_| ModelRegistryError::DatabaseFailed)?;

            if active.is_none() {
                store.insert_active_config(config).await?;
            }
        }

        Ok(())
    }
}

impl PostgresModelRegistry {
    async fn add_instance_token_for_instance(
        &self,
        instance_id: &str,
        token: &str,
    ) -> Result<(), ModelRegistryError> {
        sqlx::query(
            "insert into instance_tokens (id, hermes_instance_id, token_hash, status) \
             values ($1, $2, $3, 'active') \
             on conflict (token_hash) do update set \
               hermes_instance_id = excluded.hermes_instance_id, status = 'active', revoked_at = null",
        )
        .bind(Uuid::new_v4())
        .bind(parse_uuid(instance_id)?)
        .bind(hash_token(token))
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        Ok(())
    }

    async fn revoke_instance_tokens_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<(), ModelRegistryError> {
        sqlx::query(
            "update instance_tokens set status = 'revoked', revoked_at = now() \
             where hermes_instance_id = $1::uuid and status = 'active'",
        )
        .bind(parse_uuid(instance_id)?)
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        Ok(())
    }

    async fn instance_token_context(&self, token: &str) -> Option<InstanceTokenContext> {
        let row = sqlx::query(
            "select instance_tokens.hermes_instance_id::text as hermes_instance_id, \
                    hermes_instances.user_id::text as user_id \
             from instance_tokens \
             join hermes_instances on hermes_instances.id = instance_tokens.hermes_instance_id \
             where instance_tokens.token_hash = $1 and instance_tokens.status = 'active'",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await;

        row.ok().flatten().map(|row| InstanceTokenContext {
            hermes_instance_id: row.try_get("hermes_instance_id").ok(),
            user_id: row.try_get("user_id").ok(),
        })
    }

    async fn active_config(&self, kind: &str) -> Result<ModelConfig, ModelRegistryError> {
        validate_config_kind(kind)?;
        let row = sqlx::query(
                "select config_kind, provider_name, provider_base_url, provider_api_key_secret_ref, default_model, \
                    allowed_models, api_type, reasoning_effort, allow_streaming, request_timeout_seconds \
             from model_configs where is_active = true and config_kind = $1 \
             order by updated_at desc limit 1",
        )
        .bind(kind)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        row_to_model_config(&row, &self.cipher)
    }

    async fn replace(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        validate_config_kind(&config.config_kind)?;
        let config = normalize_model_config(config)?;
        let encrypted_key = encrypt_secret(&self.cipher, &config.provider_api_key);
        let updated = sqlx::query(
            "update model_configs set \
               provider_name = $1, provider_base_url = $2, provider_api_key_secret_ref = $3, \
               default_model = $4, allowed_models = $5, api_type = $6, reasoning_effort = $7, \
               allow_streaming = $8, request_timeout_seconds = $9, updated_at = now() \
             where is_active = true and config_kind = $10",
        )
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(&config.api_type)
        .bind(&config.reasoning_effort)
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .bind(&config.config_kind)
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        if updated.rows_affected() == 0 {
            self.insert_active_config(config).await?;
        }

        Ok(())
    }

    async fn insert_active_config(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        validate_config_kind(&config.config_kind)?;
        let config = normalize_model_config(config)?;
        let encrypted_key = encrypt_secret(&self.cipher, &config.provider_api_key);
        sqlx::query(
            "insert into model_configs \
             (id, config_kind, provider_name, provider_base_url, provider_api_key_secret_ref, default_model, allowed_models, api_type, reasoning_effort, allow_streaming, request_timeout_seconds, is_active) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true)",
        )
        .bind(Uuid::new_v4())
        .bind(&config.config_kind)
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(&config.api_type)
        .bind(&config.reasoning_effort)
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        Ok(())
    }
}

pub fn is_runtime_model_config_ready(config: &ModelConfig) -> bool {
    // 默认配置只是占位，管理员必须显式保存真实 provider 信息后才能开放用户和 Hermes。
    let provider_name = config.provider_name.trim();
    let provider_base_url = config.provider_base_url.trim();
    let provider_api_key = config.provider_api_key.trim();
    let default_model = config.default_model.trim();

    !provider_name.is_empty()
        && !provider_base_url.is_empty()
        && !provider_api_key.is_empty()
        && !default_model.is_empty()
        && config.request_timeout_seconds > 0
        && provider_base_url != "https://provider.example/v1"
        && !matches!(
            provider_api_key,
            "provider-secret" | "replace-with-provider-key" | "changeme"
        )
}

fn row_to_model_config(
    row: &sqlx::postgres::PgRow,
    cipher: &SecretCipher,
) -> Result<ModelConfig, ModelRegistryError> {
    let encrypted_key = row
        .try_get::<String, _>("provider_api_key_secret_ref")
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;
    let allowed_models = row
        .try_get::<Value, _>("allowed_models")
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;
    let config_kind = row
        .try_get::<String, _>("config_kind")
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;
    let api_type = row
        .try_get::<String, _>("api_type")
        .unwrap_or_else(|_| default_api_type_for_kind(&config_kind).to_string());
    let reasoning_effort = row
        .try_get::<Option<String>, _>("reasoning_effort")
        .unwrap_or(None);

    normalize_model_config(ModelConfig {
        config_kind,
        provider_name: row
            .try_get("provider_name")
            .map_err(|_| ModelRegistryError::DatabaseFailed)?,
        provider_base_url: row
            .try_get("provider_base_url")
            .map_err(|_| ModelRegistryError::DatabaseFailed)?,
        provider_api_key: decrypt_config_secret(cipher, &encrypted_key)?,
        default_model: row
            .try_get("default_model")
            .map_err(|_| ModelRegistryError::DatabaseFailed)?,
        allowed_models: serde_json::from_value(allowed_models)
            .map_err(|_| ModelRegistryError::InvalidRequest)?,
        api_type,
        reasoning_effort,
        allow_streaming: row
            .try_get("allow_streaming")
            .map_err(|_| ModelRegistryError::DatabaseFailed)?,
        request_timeout_seconds: u64::try_from(
            row.try_get::<i32, _>("request_timeout_seconds")
                .map_err(|_| ModelRegistryError::DatabaseFailed)?,
        )
        .map_err(|_| ModelRegistryError::InvalidRequest)?,
    })
}

fn default_config_set(config: ModelConfig) -> HashMap<String, ModelConfig> {
    MODEL_CONFIG_KINDS
        .into_iter()
        .map(|kind| {
            let config = default_config_for_kind(&config, kind);
            (kind.to_string(), config)
        })
        .collect()
}

fn default_config_for_kind(base: &ModelConfig, kind: &str) -> ModelConfig {
    let mut config = base.clone();
    config.config_kind = kind.to_string();
    config.api_type = default_api_type_for_kind(kind).to_string();

    // 图片生成和标题生成第一版复用同一个 provider 接入形态，但各自保存独立模型名。
    if kind == IMAGE_MODEL_CONFIG_KIND && config.default_model == "gpt-4.1-mini" {
        config.default_model = "gpt-image-1".to_string();
    }
    if kind != LLM_MODEL_CONFIG_KIND {
        config.allow_streaming = false;
        config.allowed_models = vec![config.default_model.clone()];
    }

    normalize_model_config(config).unwrap_or_else(|_| base.clone())
}

fn normalize_model_config(mut config: ModelConfig) -> Result<ModelConfig, ModelRegistryError> {
    validate_config_kind(&config.config_kind)?;
    if config.allowed_models.is_empty() {
        config.allowed_models = vec![config.default_model.clone()];
    }
    if config.api_type.trim().is_empty() {
        config.api_type = default_api_type_for_kind(&config.config_kind).to_string();
    }
    if config.config_kind == IMAGE_MODEL_CONFIG_KIND {
        config.api_type = IMAGES_GENERATIONS_API_TYPE.to_string();
        config.reasoning_effort = None;
        config.allow_streaming = false;
    }
    validate_api_type_for_kind(&config.config_kind, &config.api_type)?;
    config.reasoning_effort = normalize_reasoning_effort(config.reasoning_effort)?;
    Ok(config)
}

fn sanitize_image_generation_request(object: &mut Map<String, Value>) {
    // Hermes 的 OpenAI 图片插件会附带 quality 这一类 OpenAI 专有参数；
    // 许多 OpenAI-compatible 网关会直接 502。Hub 以管理员的图片模型配置为准，
    // 第一版只转发通用图片生成字段，保证兼容性优先。
    for key in ["quality", "background", "output_format", "moderation"] {
        object.remove(key);
    }
}

fn apply_reasoning_config(object: &mut Map<String, Value>, config: &ModelConfig, api_type: &str) {
    let Some(effort) = config.reasoning_effort.as_deref() else {
        return;
    };

    // OpenAI Chat Completions 使用顶层 reasoning_effort；Responses 使用 reasoning.effort。
    if api_type == RESPONSES_API_TYPE {
        let mut reasoning = object
            .remove("reasoning")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        reasoning.insert("effort".to_string(), Value::String(effort.to_string()));
        object.insert("reasoning".to_string(), Value::Object(reasoning));
    } else {
        object.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.to_string()),
        );
    }
}

fn validate_config_kind(kind: &str) -> Result<(), ModelRegistryError> {
    if MODEL_CONFIG_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(ModelRegistryError::InvalidRequest)
    }
}

pub fn default_api_type_for_kind(kind: &str) -> &'static str {
    if kind == IMAGE_MODEL_CONFIG_KIND {
        IMAGES_GENERATIONS_API_TYPE
    } else {
        CHAT_COMPLETIONS_API_TYPE
    }
}

pub fn validate_api_type_for_kind(kind: &str, api_type: &str) -> Result<(), ModelRegistryError> {
    validate_config_kind(kind)?;
    let valid = if kind == IMAGE_MODEL_CONFIG_KIND {
        api_type == IMAGES_GENERATIONS_API_TYPE
    } else {
        matches!(api_type, CHAT_COMPLETIONS_API_TYPE | RESPONSES_API_TYPE)
    };

    if valid {
        Ok(())
    } else {
        Err(ModelRegistryError::InvalidRequest)
    }
}

pub fn normalize_reasoning_effort(
    effort: Option<String>,
) -> Result<Option<String>, ModelRegistryError> {
    let Some(effort) = effort.map(|value| value.trim().to_ascii_lowercase()) else {
        return Ok(None);
    };
    if effort.is_empty() || effort == "off" || effort == "none" {
        return Ok(None);
    }
    if REASONING_EFFORTS.contains(&effort.as_str()) {
        Ok(Some(effort))
    } else {
        Err(ModelRegistryError::InvalidRequest)
    }
}

fn random_token() -> Result<String, ModelRegistryError> {
    let mut token_bytes = [0u8; 32];
    getrandom_fill(&mut token_bytes).map_err(|_| ModelRegistryError::InvalidRequest)?;
    Ok(URL_SAFE_NO_PAD.encode(token_bytes))
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn parse_uuid(value: &str) -> Result<Uuid, ModelRegistryError> {
    Uuid::parse_str(value).map_err(|_| ModelRegistryError::InvalidRequest)
}

fn timeout_as_i32(value: u64) -> Result<i32, ModelRegistryError> {
    i32::try_from(value).map_err(|_| ModelRegistryError::InvalidRequest)
}

fn decrypt_config_secret(
    cipher: &SecretCipher,
    secret_ref: &str,
) -> Result<String, ModelRegistryError> {
    if !secret_ref.starts_with("v1.") {
        return Ok(secret_ref.to_string());
    }

    decrypt_secret(cipher, secret_ref).map_err(|_| ModelRegistryError::SecretFailed)
}
