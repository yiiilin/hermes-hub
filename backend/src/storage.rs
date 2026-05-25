use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::TryStreamExt;
use object_store::{aws::AmazonS3Builder, path::Path as ObjectPath, ObjectStore, ObjectStoreExt};
use thiserror::Error;

use crate::app_config::ObjectStorageConfig;

pub type DynObjectStorage = Arc<dyn HubObjectStorage>;

#[derive(Debug, Error)]
pub enum ObjectStorageError {
    #[error("object storage lock failed")]
    LockFailed,
    #[error("object not found")]
    NotFound,
    #[error("object storage operation failed")]
    OperationFailed,
    #[error("object storage is not configured")]
    NotConfigured,
}

#[async_trait]
pub trait HubObjectStorage: Send + Sync + 'static {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectStorageError>;
    async fn get(&self, key: &str) -> Result<Bytes, ObjectStorageError>;
    async fn delete(&self, key: &str) -> Result<(), ObjectStorageError>;
    async fn list_prefix(&self, prefix: &str) -> Result<Vec<ObjectInfo>, ObjectStorageError>;
    fn bucket(&self) -> &str;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
}

#[derive(Clone)]
pub struct InMemoryObjectStorage {
    bucket: String,
    objects: Arc<Mutex<HashMap<String, Bytes>>>,
}

impl InMemoryObjectStorage {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            objects: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn shared(self) -> DynObjectStorage {
        Arc::new(self)
    }
}

impl Default for InMemoryObjectStorage {
    fn default() -> Self {
        Self::new("hermes-hub-test")
    }
}

#[async_trait]
impl HubObjectStorage for InMemoryObjectStorage {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectStorageError> {
        self.objects
            .lock()
            .map_err(|_| ObjectStorageError::LockFailed)?
            .insert(key.to_string(), bytes);
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, ObjectStorageError> {
        self.objects
            .lock()
            .map_err(|_| ObjectStorageError::LockFailed)?
            .get(key)
            .cloned()
            .ok_or(ObjectStorageError::NotFound)
    }

    async fn delete(&self, key: &str) -> Result<(), ObjectStorageError> {
        self.objects
            .lock()
            .map_err(|_| ObjectStorageError::LockFailed)?
            .remove(key);
        Ok(())
    }

    async fn list_prefix(&self, prefix: &str) -> Result<Vec<ObjectInfo>, ObjectStorageError> {
        let mut objects = self
            .objects
            .lock()
            .map_err(|_| ObjectStorageError::LockFailed)?
            .iter()
            .filter_map(|(key, bytes)| {
                key.starts_with(prefix).then(|| ObjectInfo {
                    key: key.clone(),
                    size: bytes.len() as u64,
                })
            })
            .collect::<Vec<_>>();
        objects.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(objects)
    }

    fn bucket(&self) -> &str {
        &self.bucket
    }
}

pub struct S3ObjectStorage {
    bucket: String,
    store: Arc<dyn ObjectStore>,
}

impl S3ObjectStorage {
    pub fn from_config(config: &ObjectStorageConfig) -> Result<Self, ObjectStorageError> {
        let endpoint = config
            .endpoint
            .clone()
            .ok_or(ObjectStorageError::NotConfigured)?;
        let access_key = config
            .access_key
            .clone()
            .ok_or(ObjectStorageError::NotConfigured)?;
        let secret_key = config
            .secret_key
            .clone()
            .ok_or(ObjectStorageError::NotConfigured)?;

        let store = AmazonS3Builder::new()
            .with_endpoint(endpoint.clone())
            .with_allow_http(endpoint.starts_with("http://"))
            .with_virtual_hosted_style_request(!config.force_path_style)
            .with_region(config.region.clone())
            .with_bucket_name(config.bucket.clone())
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key)
            .build()
            .map_err(|error| {
                tracing::warn!(
                    endpoint = %endpoint,
                    bucket = %config.bucket,
                    error = %error,
                    "failed to initialize S3 object storage"
                );
                ObjectStorageError::OperationFailed
            })?;

        Ok(Self {
            bucket: config.bucket.clone(),
            store: Arc::new(store),
        })
    }

    pub fn shared(self) -> DynObjectStorage {
        Arc::new(self)
    }
}

#[async_trait]
impl HubObjectStorage for S3ObjectStorage {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), ObjectStorageError> {
        self.store
            .put(&ObjectPath::from(key), bytes.into())
            .await
            .map_err(|error| {
                tracing::warn!(
                    bucket = %self.bucket,
                    key = %key,
                    error = %error,
                    "object storage put failed"
                );
                ObjectStorageError::OperationFailed
            })?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, ObjectStorageError> {
        self.store
            .get(&ObjectPath::from(key))
            .await
            .map_err(|error| {
                if matches!(error, object_store::Error::NotFound { .. }) {
                    ObjectStorageError::NotFound
                } else {
                    tracing::warn!(
                        bucket = %self.bucket,
                        key = %key,
                        error = %error,
                        "object storage get failed"
                    );
                    ObjectStorageError::OperationFailed
                }
            })?
            .bytes()
            .await
            .map_err(|error| {
                tracing::warn!(
                    bucket = %self.bucket,
                    key = %key,
                    error = %error,
                    "object storage read failed"
                );
                ObjectStorageError::OperationFailed
            })
    }

    async fn delete(&self, key: &str) -> Result<(), ObjectStorageError> {
        self.store
            .delete(&ObjectPath::from(key))
            .await
            .map_err(|error| {
                tracing::warn!(
                    bucket = %self.bucket,
                    key = %key,
                    error = %error,
                    "object storage delete failed"
                );
                ObjectStorageError::OperationFailed
            })
    }

    async fn list_prefix(&self, prefix: &str) -> Result<Vec<ObjectInfo>, ObjectStorageError> {
        let mut objects = self
            .store
            .list(Some(&ObjectPath::from(prefix)))
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| {
                tracing::warn!(
                    bucket = %self.bucket,
                    prefix = %prefix,
                    error = %error,
                    "object storage list failed"
                );
                ObjectStorageError::OperationFailed
            })?
            .into_iter()
            .map(|metadata| ObjectInfo {
                key: metadata.location.to_string(),
                size: metadata.size,
            })
            .collect::<Vec<_>>();
        objects.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(objects)
    }

    fn bucket(&self) -> &str {
        &self.bucket
    }
}

pub fn object_storage_from_config(config: &ObjectStorageConfig) -> DynObjectStorage {
    if config.endpoint.is_some() && config.access_key.is_some() && config.secret_key.is_some() {
        if let Ok(storage) = S3ObjectStorage::from_config(config) {
            return storage.shared();
        } else {
            tracing::warn!(
                endpoint = ?config.endpoint,
                bucket = %config.bucket,
                "failed to initialize configured S3 object storage; falling back to memory"
            );
        }
    }

    // 未配置 S3 时使用内存实现，便于测试和本地快速启动；部署 compose 默认接 RustFS。
    InMemoryObjectStorage::new(config.bucket.clone()).shared()
}

pub fn session_object_key(
    prefix: &str,
    user_id: &str,
    session_id: &str,
    attachment_id: &str,
    file_name: &str,
) -> String {
    let prefix = prefix.trim_matches('/');
    let file_name = safe_file_name(file_name);
    format!("{prefix}/users/{user_id}/sessions/{session_id}/{attachment_id}/{file_name}")
}

fn safe_file_name(file_name: &str) -> String {
    let sanitized = file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .trim_matches('_')
        .to_string();

    if sanitized.is_empty() {
        "attachment.bin".to_string()
    } else {
        sanitized.chars().take(120).collect()
    }
}
