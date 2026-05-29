use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use getrandom::fill as getrandom_fill;
use serde::{Deserialize, Serialize};
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
    TITLE_MODEL_CONFIG_KIND,
    IMAGE_MODEL_CONFIG_KIND,
];
pub const REQUIRED_RUNTIME_MODEL_CONFIG_KINDS: [&str; 2] =
    [LLM_MODEL_CONFIG_KIND, TITLE_MODEL_CONFIG_KIND];
pub const CHAT_COMPLETIONS_API_TYPE: &str = "chat_completions";
pub const RESPONSES_API_TYPE: &str = "responses";
pub const IMAGES_GENERATIONS_API_TYPE: &str = "images_generations";
pub const REASONING_EFFORTS: [&str; 4] = ["minimal", "low", "medium", "high"];
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 4096;
pub const DEFAULT_TEMPERATURE: f64 = 0.7;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelConfig {
    pub config_kind: String,
    pub enabled: bool,
    pub provider_name: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub api_type: String,
    pub reasoning_effort: Option<String>,
    pub allow_streaming: bool,
    pub request_timeout_seconds: u64,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
    pub fallback: Option<ModelFallbackConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFallbackConfig {
    pub enabled: bool,
    pub provider_name: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub api_type: String,
    pub reasoning_effort: Option<String>,
    pub allow_streaming: bool,
    pub request_timeout_seconds: u64,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
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

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModelRequest {
    pub config: ModelConfig,
    pub body: Vec<u8>,
    pub model: String,
    pub fallback: Option<PreparedFallbackModelRequest>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedFallbackModelRequest {
    pub config: ModelFallbackConfig,
    pub body: Vec<u8>,
    pub model: String,
}

#[derive(Debug, Error)]
pub enum ModelRegistryError {
    #[error("model registry lock failed")]
    LockFailed,
    #[error("model config is disabled")]
    ModelDisabled,
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
            enabled: true,
            allow_streaming: true,
            request_timeout_seconds: 60,
            context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            temperature: DEFAULT_TEMPERATURE,
            supports_parallel_tools: true,
            fallback: None,
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
        if !config.enabled {
            return Err(ModelRegistryError::ModelDisabled);
        }
        validate_api_type_for_kind(config_kind, api_type)?;
        let original_body = body.clone();
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
            apply_runtime_model_defaults(object, &config, api_type);
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
        let fallback = prepare_fallback_request_body(
            original_body,
            config_kind,
            api_type,
            config.fallback.as_ref(),
        )?;

        Ok(PreparedModelRequest {
            config,
            body: bytes,
            model,
            fallback,
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
                "select config_kind, enabled, provider_name, provider_base_url, provider_api_key_secret_ref, default_model, \
                    allowed_models, api_type, reasoning_effort, allow_streaming, request_timeout_seconds, \
                    context_window_tokens, max_output_tokens, temperature, supports_parallel_tools, fallback_config \
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
        let fallback_config = encrypt_fallback_config(&self.cipher, &config.fallback)?;
        let updated = sqlx::query(
            "update model_configs set \
               enabled = $1, provider_name = $2, provider_base_url = $3, provider_api_key_secret_ref = $4, \
               default_model = $5, allowed_models = $6, api_type = $7, reasoning_effort = $8, \
               allow_streaming = $9, request_timeout_seconds = $10, context_window_tokens = $11, \
               max_output_tokens = $12, temperature = $13, supports_parallel_tools = $14, \
               fallback_config = $15, updated_at = now() \
             where is_active = true and config_kind = $16",
        )
        .bind(config.enabled)
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(&config.api_type)
        .bind(&config.reasoning_effort)
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .bind(tokens_as_i64(config.context_window_tokens)?)
        .bind(tokens_as_i64(config.max_output_tokens)?)
        .bind(config.temperature)
        .bind(config.supports_parallel_tools)
        .bind(fallback_config)
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
        let fallback_config = encrypt_fallback_config(&self.cipher, &config.fallback)?;
        sqlx::query(
            "insert into model_configs \
             (id, config_kind, enabled, provider_name, provider_base_url, provider_api_key_secret_ref, default_model, allowed_models, api_type, reasoning_effort, allow_streaming, request_timeout_seconds, context_window_tokens, max_output_tokens, temperature, supports_parallel_tools, fallback_config, is_active) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, true)",
        )
        .bind(Uuid::new_v4())
        .bind(&config.config_kind)
        .bind(config.enabled)
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(&config.api_type)
        .bind(&config.reasoning_effort)
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .bind(tokens_as_i64(config.context_window_tokens)?)
        .bind(tokens_as_i64(config.max_output_tokens)?)
        .bind(config.temperature)
        .bind(config.supports_parallel_tools)
        .bind(fallback_config)
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

    config.enabled
        && !provider_name.is_empty()
        && !provider_base_url.is_empty()
        && !provider_api_key.is_empty()
        && !default_model.is_empty()
        && config.request_timeout_seconds > 0
        && config.context_window_tokens > 0
        && config.max_output_tokens > 0
        && (0.0..=2.0).contains(&config.temperature)
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
    let fallback = decrypt_fallback_config(
        cipher,
        row.try_get::<Option<Value>, _>("fallback_config")
            .unwrap_or(None),
    )?;

    normalize_model_config(ModelConfig {
        config_kind,
        enabled: row
            .try_get("enabled")
            .map_err(|_| ModelRegistryError::DatabaseFailed)?,
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
        context_window_tokens: u64::try_from(
            row.try_get::<i64, _>("context_window_tokens")
                .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS as i64),
        )
        .map_err(|_| ModelRegistryError::InvalidRequest)?,
        max_output_tokens: u64::try_from(
            row.try_get::<i64, _>("max_output_tokens")
                .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS as i64),
        )
        .map_err(|_| ModelRegistryError::InvalidRequest)?,
        temperature: row
            .try_get::<f64, _>("temperature")
            .unwrap_or(DEFAULT_TEMPERATURE),
        supports_parallel_tools: row.try_get("supports_parallel_tools").unwrap_or(true),
        fallback,
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
    // 图片生成不是所有部署都有可用供应商；必须由管理员显式开启。
    config.enabled = kind != IMAGE_MODEL_CONFIG_KIND;
    config.api_type = default_api_type_for_kind(kind).to_string();

    // 图片生成和标题生成第一版复用同一个 provider 接入形态，但各自保存独立模型名。
    if kind == IMAGE_MODEL_CONFIG_KIND && config.default_model == "gpt-4.1-mini" {
        config.default_model = "gpt-image-1".to_string();
    }
    if kind != LLM_MODEL_CONFIG_KIND {
        config.allow_streaming = false;
        config.allowed_models = vec![config.default_model.clone()];
        config.supports_parallel_tools = false;
    }
    if kind == IMAGE_MODEL_CONFIG_KIND {
        config.fallback = None;
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
        config.supports_parallel_tools = false;
    } else {
        // 大模型和标题模型是运行时必需配置，不暴露关闭开关。
        config.enabled = true;
    }
    config.fallback = normalize_model_fallback_config(&config.config_kind, config.fallback)?;
    if config.context_window_tokens == 0 {
        config.context_window_tokens = DEFAULT_CONTEXT_WINDOW_TOKENS;
    }
    if config.max_output_tokens == 0 {
        config.max_output_tokens = DEFAULT_MAX_OUTPUT_TOKENS;
    }
    if !(0.0..=2.0).contains(&config.temperature) {
        return Err(ModelRegistryError::InvalidRequest);
    }
    validate_api_type_for_kind(&config.config_kind, &config.api_type)?;
    config.reasoning_effort = normalize_reasoning_effort(config.reasoning_effort)?;
    Ok(config)
}

pub fn normalize_model_fallback_config(
    config_kind: &str,
    fallback: Option<ModelFallbackConfig>,
) -> Result<Option<ModelFallbackConfig>, ModelRegistryError> {
    validate_config_kind(config_kind)?;
    if config_kind == IMAGE_MODEL_CONFIG_KIND {
        return Ok(None);
    }
    let Some(mut fallback) = fallback else {
        return Ok(None);
    };

    if fallback.api_type.trim().is_empty() {
        fallback.api_type = default_api_type_for_kind(config_kind).to_string();
    }
    validate_api_type_for_kind(config_kind, &fallback.api_type)?;
    fallback.reasoning_effort = normalize_reasoning_effort(fallback.reasoning_effort)?;
    if fallback.request_timeout_seconds == 0 {
        fallback.request_timeout_seconds = 60;
    }
    if fallback.context_window_tokens == 0 {
        fallback.context_window_tokens = DEFAULT_CONTEXT_WINDOW_TOKENS;
    }
    if fallback.max_output_tokens == 0 {
        fallback.max_output_tokens = DEFAULT_MAX_OUTPUT_TOKENS;
    }
    if !(0.0..=2.0).contains(&fallback.temperature) {
        return Err(ModelRegistryError::InvalidRequest);
    }

    if fallback.allowed_models.is_empty() && !fallback.default_model.trim().is_empty() {
        fallback.allowed_models = vec![fallback.default_model.clone()];
    }
    if fallback.enabled {
        let has_required_provider = !fallback.provider_name.trim().is_empty()
            && !fallback.provider_base_url.trim().is_empty()
            && !fallback.provider_api_key.trim().is_empty()
            && !fallback.default_model.trim().is_empty();
        if !has_required_provider {
            return Err(ModelRegistryError::InvalidRequest);
        }
        if !fallback
            .allowed_models
            .iter()
            .any(|allowed| allowed == &fallback.default_model)
        {
            fallback.allowed_models.push(fallback.default_model.clone());
        }
    }

    Ok(Some(fallback))
}

fn prepare_fallback_request_body(
    mut body: Value,
    config_kind: &str,
    api_type: &str,
    fallback: Option<&ModelFallbackConfig>,
) -> Result<Option<PreparedFallbackModelRequest>, ModelRegistryError> {
    if config_kind == IMAGE_MODEL_CONFIG_KIND {
        return Ok(None);
    }
    let Some(config) = fallback.filter(|config| config.enabled) else {
        return Ok(None);
    };
    // 代理入口的 path 已由 Hermes 当前请求决定；fallback 只在同一种 API 形态下接管，
    // 避免把 chat body 直接投递给 responses provider 造成更隐蔽的兼容问题。
    if config.api_type != api_type {
        return Ok(None);
    }
    let object = body
        .as_object_mut()
        .ok_or(ModelRegistryError::InvalidRequest)?;
    let model = config.default_model.clone();
    if !config
        .allowed_models
        .iter()
        .any(|allowed| allowed == &model)
    {
        return Err(ModelRegistryError::ModelNotAllowed);
    }

    object.insert("model".to_string(), Value::String(model.clone()));
    apply_fallback_reasoning_config(object, config, api_type);
    apply_fallback_runtime_model_defaults(object, config, api_type);
    if object
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        && !config.allow_streaming
    {
        return Ok(None);
    }

    let bytes = serde_json::to_vec(&Value::Object(object.clone()))
        .map_err(|_| ModelRegistryError::InvalidRequest)?;

    Ok(Some(PreparedFallbackModelRequest {
        config: config.clone(),
        body: bytes,
        model,
    }))
}

fn sanitize_image_generation_request(object: &mut Map<String, Value>) {
    // quality/background/output_format 是图片输出控制参数，管理员已允许透传给上游。
    // moderation 更偏供应商审核策略，兼容网关支持不稳定，暂时继续清洗。
    for key in ["moderation"] {
        object.remove(key);
    }
}

fn apply_reasoning_config(object: &mut Map<String, Value>, config: &ModelConfig, api_type: &str) {
    apply_reasoning_effort_config(object, config.reasoning_effort.as_deref(), api_type);
}

fn apply_fallback_reasoning_config(
    object: &mut Map<String, Value>,
    config: &ModelFallbackConfig,
    api_type: &str,
) {
    apply_reasoning_effort_config(object, config.reasoning_effort.as_deref(), api_type);
}

fn apply_reasoning_effort_config(
    object: &mut Map<String, Value>,
    effort: Option<&str>,
    api_type: &str,
) {
    let Some(effort) = effort else {
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

fn apply_runtime_model_defaults(
    object: &mut Map<String, Value>,
    config: &ModelConfig,
    api_type: &str,
) {
    // 这里给 Hub 代理后的主模型请求补管理员默认值；调用方显式要求更小输出时会被保留。
    let output_key = if api_type == RESPONSES_API_TYPE {
        "max_output_tokens"
    } else {
        "max_tokens"
    };
    let requested_output_tokens = object.get(output_key).and_then(Value::as_u64);
    let output_tokens = requested_output_tokens
        .map(|requested| requested.min(config.max_output_tokens))
        .unwrap_or(config.max_output_tokens);
    object.insert(output_key.to_string(), json!(output_tokens));

    object
        .entry("temperature".to_string())
        .or_insert_with(|| json!(config.temperature));

    if config.supports_parallel_tools {
        object
            .entry("parallel_tool_calls".to_string())
            .or_insert(Value::Bool(true));
    } else {
        object.remove("parallel_tool_calls");
    }
}

fn apply_fallback_runtime_model_defaults(
    object: &mut Map<String, Value>,
    config: &ModelFallbackConfig,
    api_type: &str,
) {
    let output_key = if api_type == RESPONSES_API_TYPE {
        "max_output_tokens"
    } else {
        "max_tokens"
    };
    let requested_output_tokens = object.get(output_key).and_then(Value::as_u64);
    let output_tokens = requested_output_tokens
        .map(|requested| requested.min(config.max_output_tokens))
        .unwrap_or(config.max_output_tokens);
    object.insert(output_key.to_string(), json!(output_tokens));

    object
        .entry("temperature".to_string())
        .or_insert_with(|| json!(config.temperature));

    if config.supports_parallel_tools {
        object
            .entry("parallel_tool_calls".to_string())
            .or_insert(Value::Bool(true));
    } else {
        object.remove("parallel_tool_calls");
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

fn tokens_as_i64(value: u64) -> Result<i64, ModelRegistryError> {
    i64::try_from(value).map_err(|_| ModelRegistryError::InvalidRequest)
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

fn encrypt_fallback_config(
    cipher: &SecretCipher,
    fallback: &Option<ModelFallbackConfig>,
) -> Result<Option<Value>, ModelRegistryError> {
    let Some(fallback) = fallback else {
        return Ok(None);
    };
    let mut value =
        serde_json::to_value(fallback).map_err(|_| ModelRegistryError::InvalidRequest)?;
    if let Some(object) = value.as_object_mut() {
        // fallback 作为 JSON 保存，内部 API key 仍按主配置一样加密，避免明文落库。
        if let Some(api_key) = object.get("provider_api_key").and_then(Value::as_str) {
            object.insert(
                "provider_api_key".to_string(),
                Value::String(encrypt_secret(cipher, api_key)),
            );
        }
    }
    Ok(Some(value))
}

fn decrypt_fallback_config(
    cipher: &SecretCipher,
    fallback: Option<Value>,
) -> Result<Option<ModelFallbackConfig>, ModelRegistryError> {
    let Some(mut value) = fallback else {
        return Ok(None);
    };
    if let Some(object) = value.as_object_mut() {
        if let Some(api_key) = object
            .get("provider_api_key")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        {
            object.insert(
                "provider_api_key".to_string(),
                Value::String(decrypt_config_secret(cipher, &api_key)?),
            );
        }
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| ModelRegistryError::InvalidRequest)
}
