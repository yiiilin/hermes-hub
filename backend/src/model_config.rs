use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelConfig {
    pub provider_name: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub allow_streaming: bool,
    pub request_timeout_seconds: u64,
}

#[derive(Clone)]
pub struct ModelRegistry {
    inner: Arc<Mutex<ModelRegistryInner>>,
}

#[derive(Clone)]
struct ModelRegistryInner {
    config: ModelConfig,
    instance_token_hashes: HashSet<String>,
}

#[derive(Debug, Error)]
pub enum ModelRegistryError {
    #[error("model registry lock failed")]
    LockFailed,
    #[error("model is not allowed")]
    ModelNotAllowed,
    #[error("invalid model request")]
    InvalidRequest,
}

impl ModelRegistry {
    pub fn new(config: ModelConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ModelRegistryInner {
                config,
                instance_token_hashes: HashSet::new(),
            })),
        }
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
        if let Ok(mut inner) = self.inner.lock() {
            inner.instance_token_hashes.insert(hash_token(token));
        }
    }

    pub fn verify_instance_token(&self, token: &str) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.instance_token_hashes.contains(&hash_token(token)))
            .unwrap_or(false)
    }

    pub fn active_config(&self) -> Result<ModelConfig, ModelRegistryError> {
        self.inner
            .lock()
            .map(|inner| inner.config.clone())
            .map_err(|_| ModelRegistryError::LockFailed)
    }

    pub fn models_payload(&self) -> Result<serde_json::Value, ModelRegistryError> {
        let config = self.active_config()?;
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

    pub fn prepare_request_body(
        &self,
        mut body: serde_json::Value,
    ) -> Result<(ModelConfig, Vec<u8>), ModelRegistryError> {
        let config = self.active_config()?;
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

        object.insert("model".to_string(), serde_json::Value::String(model));
        let bytes = serde_json::to_vec(&serde_json::Value::Object(object.clone()))
            .map_err(|_| ModelRegistryError::InvalidRequest)?;

        Ok((config, bytes))
    }
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}
