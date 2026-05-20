use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use getrandom::fill as getrandom_fill;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::security::crypto::{decrypt_secret, encrypt_secret, SecretCipher};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelConfig {
    pub provider_name: String,
    pub provider_base_url: String,
    #[serde(skip_serializing)]
    pub provider_api_key: String,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub allow_streaming: bool,
    pub request_timeout_seconds: u64,
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
    config: ModelConfig,
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
        Self {
            backend: ModelRegistryBackend::Memory(Arc::new(Mutex::new(ModelRegistryInner {
                config,
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
            provider_name: "openai-compatible".to_string(),
            provider_base_url: "https://provider.example/v1".to_string(),
            provider_api_key: "provider-secret".to_string(),
            default_model: "gpt-4.1-mini".to_string(),
            allowed_models: vec!["gpt-4.1-mini".to_string()],
            allow_streaming: true,
            request_timeout_seconds: 60,
        })
    }

    pub fn add_instance_token(&self, token: &str) {
        if let ModelRegistryBackend::Memory(inner) = &self.backend {
            if let Ok(mut inner) = inner.lock() {
                inner.instance_tokens_by_hash.insert(hash_token(token), None);
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
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => inner
                .lock()
                .map(|inner| inner.config.clone())
                .map_err(|_| ModelRegistryError::LockFailed),
            ModelRegistryBackend::Postgres(store) => store.active_config().await,
        }
    }

    pub async fn replace(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        match &self.backend {
            ModelRegistryBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ModelRegistryError::LockFailed)?;
                inner.config = config;
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
        mut body: Value,
    ) -> Result<PreparedModelRequest, ModelRegistryError> {
        let config = self.active_config().await?;
        let object = body
            .as_object_mut()
            .ok_or(ModelRegistryError::InvalidRequest)?;
        let requested = object
            .get("model")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let model = requested.unwrap_or_else(|| config.default_model.clone());

        if !config
            .allowed_models
            .iter()
            .any(|allowed| allowed == &model)
        {
            return Err(ModelRegistryError::ModelNotAllowed);
        }

        object.insert("model".to_string(), Value::String(model.clone()));
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

        let active: Option<(Uuid,)> =
            sqlx::query_as("select id from model_configs where is_active = true limit 1")
                .fetch_optional(&store.pool)
                .await
                .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        if active.is_none() {
            store.insert_active_config(default_config).await?;
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

    async fn active_config(&self) -> Result<ModelConfig, ModelRegistryError> {
        let row = sqlx::query(
            "select provider_name, provider_base_url, provider_api_key_secret_ref, default_model, \
                    allowed_models, allow_streaming, request_timeout_seconds \
             from model_configs where is_active = true order by updated_at desc limit 1",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        row_to_model_config(&row, &self.cipher)
    }

    async fn replace(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        let encrypted_key = encrypt_secret(&self.cipher, &config.provider_api_key);
        let updated = sqlx::query(
            "update model_configs set \
               provider_name = $1, provider_base_url = $2, provider_api_key_secret_ref = $3, \
               default_model = $4, allowed_models = $5, allow_streaming = $6, \
               request_timeout_seconds = $7, updated_at = now() \
             where is_active = true",
        )
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        if updated.rows_affected() == 0 {
            self.insert_active_config(config).await?;
        }

        Ok(())
    }

    async fn insert_active_config(&self, config: ModelConfig) -> Result<(), ModelRegistryError> {
        let encrypted_key = encrypt_secret(&self.cipher, &config.provider_api_key);
        sqlx::query(
            "insert into model_configs \
             (id, provider_name, provider_base_url, provider_api_key_secret_ref, default_model, allowed_models, allow_streaming, request_timeout_seconds, is_active) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, true)",
        )
        .bind(Uuid::new_v4())
        .bind(&config.provider_name)
        .bind(&config.provider_base_url)
        .bind(&encrypted_key)
        .bind(&config.default_model)
        .bind(json!(config.allowed_models))
        .bind(config.allow_streaming)
        .bind(timeout_as_i32(config.request_timeout_seconds)?)
        .execute(&self.pool)
        .await
        .map_err(|_| ModelRegistryError::DatabaseFailed)?;

        Ok(())
    }
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

    Ok(ModelConfig {
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
