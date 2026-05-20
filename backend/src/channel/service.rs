use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::db::runtime::block_on_db;

pub const HUB_CHANNEL_NAME: &str = "hermes-hub";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelSessionKind {
    Chat,
    Agent,
}

impl ChannelSessionKind {
    pub fn parse(value: &str) -> Result<Self, ChannelStoreError> {
        match value {
            "chat" => Ok(Self::Chat),
            "agent" => Ok(Self::Agent),
            _ => Err(ChannelStoreError::InvalidSessionKind),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelMessageRole {
    User,
    Assistant,
}

impl ChannelMessageRole {
    pub fn parse(value: &str) -> Result<Self, ChannelStoreError> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            _ => Err(ChannelStoreError::InvalidMessageRole),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelAttachmentDirection {
    Input,
    Output,
}

impl ChannelAttachmentDirection {
    pub fn parse(value: &str) -> Result<Self, ChannelStoreError> {
        match value {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            _ => Err(ChannelStoreError::InvalidAttachment),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelAttachmentKind {
    File,
    Image,
}

impl ChannelAttachmentKind {
    pub fn parse(value: &str) -> Result<Self, ChannelStoreError> {
        match value {
            "file" => Ok(Self::File),
            "image" => Ok(Self::Image),
            _ => Err(ChannelStoreError::InvalidAttachment),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Channel {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelSession {
    pub id: String,
    pub channel_id: String,
    pub kind: ChannelSessionKind,
    pub hermes_session_id: Option<String>,
    pub hermes_response_id: Option<String>,
    pub hermes_run_id: Option<String>,
    pub title: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelMessage {
    pub id: String,
    pub session_id: String,
    pub role: ChannelMessageRole,
    pub content: String,
    pub attachments: Value,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelAttachment {
    pub id: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub direction: ChannelAttachmentDirection,
    pub bucket: String,
    pub object_key: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    pub kind: ChannelAttachmentKind,
    pub download_url: String,
    pub created_at: u64,
}

pub struct NewChannelAttachment {
    pub direction: ChannelAttachmentDirection,
    pub bucket: String,
    pub object_key: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    pub kind: ChannelAttachmentKind,
}

#[derive(Clone, Debug)]
pub struct ChannelSessionContext {
    pub user_id: String,
    pub channel_id: String,
    pub hermes_instance_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum ChannelStoreError {
    #[error("channel not found")]
    ChannelNotFound,
    #[error("invalid session kind")]
    InvalidSessionKind,
    #[error("invalid message role")]
    InvalidMessageRole,
    #[error("invalid attachment")]
    InvalidAttachment,
    #[error("attachment not found")]
    AttachmentNotFound,
    #[error("channel store lock failed")]
    LockFailed,
    #[error("database operation failed")]
    DatabaseFailed,
}

#[derive(Clone)]
pub struct ChannelStore {
    backend: ChannelStoreBackend,
}

#[derive(Clone)]
enum ChannelStoreBackend {
    Memory(Arc<Mutex<ChannelStoreInner>>),
    Postgres(PgPool),
}

impl Default for ChannelStore {
    fn default() -> Self {
        Self {
            backend: ChannelStoreBackend::Memory(Arc::new(
                Mutex::new(ChannelStoreInner::default()),
            )),
        }
    }
}

#[derive(Default)]
struct ChannelStoreInner {
    channels_by_id: HashMap<String, Channel>,
    sessions_by_id: HashMap<String, ChannelSession>,
    messages_by_session_id: HashMap<String, Vec<ChannelMessage>>,
    attachments_by_id: HashMap<String, ChannelAttachment>,
}

impl ChannelStore {
    pub fn postgres(pool: PgPool) -> Self {
        Self {
            backend: ChannelStoreBackend::Postgres(pool),
        }
    }

    pub async fn create_channel(
        &self,
        user_id: &str,
        name: &str,
        description: Option<String>,
    ) -> Result<Channel, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let now = unix_now();
                let channel = Channel {
                    id: Uuid::new_v4().to_string(),
                    user_id: user_id.to_string(),
                    name: name.trim().to_string(),
                    description,
                    created_at: now,
                    updated_at: now,
                };

                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .channels_by_id
                    .insert(channel.id.clone(), channel.clone());
                Ok(channel)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let instance = sqlx::query(
                    "select id::text as id from hermes_instances where user_id = $1::uuid limit 1",
                )
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                let instance_id = instance.and_then(|row| row.try_get::<String, _>("id").ok());

                let row = sqlx::query(
                    r#"
                    insert into channels (id, user_id, hermes_instance_id, name, description)
                    values ($1::uuid, $2::uuid, $3::uuid, $4, $5)
                    returning id::text as id,
                              user_id::text as user_id,
                              name,
                              description,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(user_id)
                .bind(instance_id)
                .bind(name.trim())
                .bind(description)
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                row_to_channel(&row)
            }),
        }
    }

    pub async fn list_channels(&self, user_id: &str) -> Result<Vec<Channel>, ChannelStoreError> {
        let channel = self.ensure_hub_channel(user_id).await?;
        Ok(vec![channel])
    }

    /// Hub 统一维护每个用户唯一的标准 channel，前端不再暴露创建 channel 的入口。
    pub async fn ensure_hub_channel(&self, user_id: &str) -> Result<Channel, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                if let Some(channel) = inner
                    .channels_by_id
                    .values()
                    .filter(|channel| channel.user_id == user_id)
                    .find(|channel| channel.name == HUB_CHANNEL_NAME)
                    .cloned()
                {
                    return Ok(channel);
                }

                let now = unix_now();
                let channel = Channel {
                    id: Uuid::new_v4().to_string(),
                    user_id: user_id.to_string(),
                    name: HUB_CHANNEL_NAME.to_string(),
                    description: Some("Hermes Hub default channel".to_string()),
                    created_at: now,
                    updated_at: now,
                };
                inner
                    .channels_by_id
                    .insert(channel.id.clone(), channel.clone());
                Ok(channel)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let instance = sqlx::query(
                    "select id::text as id from hermes_instances where user_id = $1::uuid limit 1",
                )
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                let instance_id = instance.and_then(|row| row.try_get::<String, _>("id").ok());

                let row = sqlx::query(
                    r#"
                    insert into channels (id, user_id, hermes_instance_id, name, description)
                    values ($1::uuid, $2::uuid, $3::uuid, $4, $5)
                    on conflict (user_id, name) do update set
                        hermes_instance_id = coalesce(excluded.hermes_instance_id, channels.hermes_instance_id),
                        updated_at = now()
                    returning id::text as id,
                              user_id::text as user_id,
                              name,
                              description,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(user_id)
                .bind(instance_id)
                .bind(HUB_CHANNEL_NAME)
                .bind(Some("Hermes Hub default channel".to_string()))
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                row_to_channel(&row)
            }),
        }
    }

    pub async fn get_channel(
        &self,
        user_id: &str,
        channel_id: &str,
    ) -> Result<Channel, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .channels_by_id
                    .get(channel_id)
                    .filter(|channel| channel.user_id == user_id)
                    .cloned()
                    .ok_or(ChannelStoreError::ChannelNotFound)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select id::text as id,
                           user_id::text as user_id,
                           name,
                           description,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at
                    from channels
                    where id = $1::uuid and user_id = $2::uuid
                    "#,
                )
                .bind(channel_id)
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::ChannelNotFound)?;

                row_to_channel(&row)
            }),
        }
    }

    pub async fn create_session(
        &self,
        user_id: &str,
        channel_id: &str,
        kind: ChannelSessionKind,
        title: Option<String>,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_channel(user_id, channel_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let now = unix_now();
                let session = ChannelSession {
                    id: Uuid::new_v4().to_string(),
                    channel_id: channel_id.to_string(),
                    kind,
                    hermes_session_id: None,
                    hermes_response_id: None,
                    hermes_run_id: None,
                    title,
                    created_at: now,
                    updated_at: now,
                };

                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .sessions_by_id
                    .insert(session.id.clone(), session.clone());
                Ok(session)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    insert into channel_sessions (id, channel_id, kind, title)
                    values ($1::uuid, $2::uuid, $3, $4)
                    returning id::text as id,
                              channel_id::text as channel_id,
                              kind,
                              hermes_session_id,
                              hermes_response_id,
                              hermes_run_id,
                              title,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(channel_id)
                .bind(kind.as_str())
                .bind(title)
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                row_to_session(&row)
            }),
        }
    }

    pub async fn list_sessions(
        &self,
        user_id: &str,
        channel_id: &str,
    ) -> Result<Vec<ChannelSession>, ChannelStoreError> {
        self.get_channel(user_id, channel_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let mut sessions = inner
                    .sessions_by_id
                    .values()
                    .filter(|session| session.channel_id == channel_id)
                    .cloned()
                    .collect::<Vec<_>>();

                sessions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
                Ok(sessions)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let rows = sqlx::query(
                    r#"
                    select id::text as id,
                           channel_id::text as channel_id,
                           kind,
                           hermes_session_id,
                           hermes_response_id,
                           hermes_run_id,
                           title,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at
                    from channel_sessions
                    where channel_id = $1::uuid
                    order by created_at desc
                    "#,
                )
                .bind(channel_id)
                .fetch_all(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                rows.iter().map(row_to_session).collect()
            }),
        }
    }

    pub async fn get_session(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_channel(user_id, channel_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .sessions_by_id
                    .get(session_id)
                    .filter(|session| session.channel_id == channel_id)
                    .cloned()
                    .ok_or(ChannelStoreError::ChannelNotFound)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select id::text as id,
                           channel_id::text as channel_id,
                           kind,
                           hermes_session_id,
                           hermes_response_id,
                           hermes_run_id,
                           title,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at
                    from channel_sessions
                    where id = $1::uuid and channel_id = $2::uuid
                    "#,
                )
                .bind(session_id)
                .bind(channel_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::ChannelNotFound)?;

                row_to_session(&row)
            }),
        }
    }

    pub async fn update_session_title(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        title: String,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_channel(user_id, channel_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let session = inner
                    .sessions_by_id
                    .get_mut(session_id)
                    .filter(|session| session.channel_id == channel_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                session.title = Some(title);
                session.updated_at = unix_now();
                Ok(session.clone())
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update channel_sessions
                    set title = $1, updated_at = now()
                    where id = $2::uuid and channel_id = $3::uuid
                    returning id::text as id,
                              channel_id::text as channel_id,
                              kind,
                              hermes_session_id,
                              hermes_response_id,
                              hermes_run_id,
                              title,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(title)
                .bind(session_id)
                .bind(channel_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::ChannelNotFound)?;

                row_to_session(&row)
            }),
        }
    }

    pub async fn list_session_messages(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<Vec<ChannelMessage>, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let mut messages = inner
                    .messages_by_session_id
                    .get(session_id)
                    .cloned()
                    .unwrap_or_default();
                messages.sort_by(|left, right| left.created_at.cmp(&right.created_at));
                Ok(messages)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let rows = sqlx::query(
                    r#"
                    select id::text as id,
                           session_id::text as session_id,
                           role,
                           content,
                           attachments,
                           extract(epoch from created_at)::bigint as created_at
                    from channel_session_messages
                    where session_id = $1::uuid
                    order by created_at asc
                    "#,
                )
                .bind(session_id)
                .fetch_all(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                rows.iter().map(row_to_message).collect()
            }),
        }
    }

    pub async fn append_session_message(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        role: ChannelMessageRole,
        content: String,
        attachments: Value,
    ) -> Result<ChannelMessage, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let now = unix_now();
                let message = ChannelMessage {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role,
                    content,
                    attachments,
                    created_at: now,
                };
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .messages_by_session_id
                    .entry(session_id.to_string())
                    .or_default()
                    .push(message.clone());
                bind_memory_attachments(&mut inner, session_id, &message.id, &message.attachments);
                Ok(message)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    insert into channel_session_messages (id, session_id, role, content, attachments)
                    values ($1::uuid, $2::uuid, $3, $4, $5)
                    returning id::text as id,
                              session_id::text as session_id,
                              role,
                              content,
                              attachments,
                              extract(epoch from created_at)::bigint as created_at
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(session_id)
                .bind(role.as_str())
                .bind(content)
                .bind(if attachments.is_null() { json!([]) } else { attachments })
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                let message = row_to_message(&row)?;
                bind_postgres_attachments(pool, session_id, &message.id, &message.attachments)
                    .await?;
                Ok(message)
            }),
        }
    }

    pub async fn create_attachment(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        input: NewChannelAttachment,
    ) -> Result<ChannelAttachment, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;
        self.create_attachment_for_session(session_id, input).await
    }

    pub async fn create_attachment_for_session(
        &self,
        session_id: &str,
        input: NewChannelAttachment,
    ) -> Result<ChannelAttachment, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                if !inner.sessions_by_id.contains_key(session_id) {
                    return Err(ChannelStoreError::ChannelNotFound);
                }
                let attachment = ChannelAttachment {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    message_id: None,
                    direction: input.direction,
                    bucket: input.bucket,
                    object_key: input.object_key,
                    name: input.name,
                    content_type: input.content_type,
                    size: input.size,
                    kind: input.kind,
                    download_url: String::new(),
                    created_at: unix_now(),
                }
                .with_download_url();
                inner
                    .attachments_by_id
                    .insert(attachment.id.clone(), attachment.clone());
                Ok(attachment)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    insert into channel_attachments (
                        id, session_id, direction, bucket, object_key, name,
                        content_type, size_bytes, kind
                    )
                    values ($1::uuid, $2::uuid, $3, $4, $5, $6, $7, $8, $9)
                    returning id::text as id,
                              session_id::text as session_id,
                              message_id::text as message_id,
                              direction,
                              bucket,
                              object_key,
                              name,
                              content_type,
                              size_bytes as size,
                              kind,
                              extract(epoch from created_at)::bigint as created_at
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(session_id)
                .bind(input.direction.as_str())
                .bind(input.bucket)
                .bind(input.object_key)
                .bind(input.name)
                .bind(input.content_type)
                .bind(input.size as i64)
                .bind(input.kind.as_str())
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                row_to_attachment(&row)
            }),
        }
    }

    pub async fn get_attachment(
        &self,
        user_id: &str,
        attachment_id: &str,
    ) -> Result<ChannelAttachment, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let attachment = inner
                    .attachments_by_id
                    .get(attachment_id)
                    .cloned()
                    .ok_or(ChannelStoreError::AttachmentNotFound)?;
                let session = inner
                    .sessions_by_id
                    .get(&attachment.session_id)
                    .ok_or(ChannelStoreError::AttachmentNotFound)?;
                let channel = inner
                    .channels_by_id
                    .get(&session.channel_id)
                    .ok_or(ChannelStoreError::AttachmentNotFound)?;
                if channel.user_id != user_id {
                    return Err(ChannelStoreError::AttachmentNotFound);
                }
                Ok(attachment)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select channel_attachments.id::text as id,
                           channel_attachments.session_id::text as session_id,
                           channel_attachments.message_id::text as message_id,
                           channel_attachments.direction,
                           channel_attachments.bucket,
                           channel_attachments.object_key,
                           channel_attachments.name,
                           channel_attachments.content_type,
                           channel_attachments.size_bytes as size,
                           channel_attachments.kind,
                           extract(epoch from channel_attachments.created_at)::bigint as created_at
                    from channel_attachments
                    join channel_sessions on channel_sessions.id = channel_attachments.session_id
                    join channels on channels.id = channel_sessions.channel_id
                    where channel_attachments.id = $1::uuid and channels.user_id = $2::uuid
                    "#,
                )
                .bind(attachment_id)
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::AttachmentNotFound)?;

                row_to_attachment(&row)
            }),
        }
    }

    pub async fn session_context(
        &self,
        session_id: &str,
    ) -> Result<ChannelSessionContext, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let session = inner
                    .sessions_by_id
                    .get(session_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                let channel = inner
                    .channels_by_id
                    .get(&session.channel_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                Ok(ChannelSessionContext {
                    user_id: channel.user_id.clone(),
                    channel_id: channel.id.clone(),
                    hermes_instance_id: None,
                })
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select channels.user_id::text as user_id,
                           channels.id::text as channel_id,
                           channels.hermes_instance_id::text as hermes_instance_id
                    from channel_sessions
                    join channels on channels.id = channel_sessions.channel_id
                    where channel_sessions.id = $1::uuid
                    "#,
                )
                .bind(session_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::ChannelNotFound)?;

                Ok(ChannelSessionContext {
                    user_id: row
                        .try_get("user_id")
                        .map_err(|_| ChannelStoreError::DatabaseFailed)?,
                    channel_id: row
                        .try_get("channel_id")
                        .map_err(|_| ChannelStoreError::DatabaseFailed)?,
                    hermes_instance_id: row
                        .try_get("hermes_instance_id")
                        .map_err(|_| ChannelStoreError::DatabaseFailed)?,
                })
            }),
        }
    }
}

fn row_to_channel(row: &sqlx::postgres::PgRow) -> Result<Channel, ChannelStoreError> {
    Ok(Channel {
        id: row
            .try_get("id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        user_id: row
            .try_get("user_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        name: row
            .try_get("name")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        description: row
            .try_get("description")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
        updated_at: row
            .try_get::<i64, _>("updated_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
    })
}

fn row_to_session(row: &sqlx::postgres::PgRow) -> Result<ChannelSession, ChannelStoreError> {
    let kind = row
        .try_get::<String, _>("kind")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;

    Ok(ChannelSession {
        id: row
            .try_get("id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        channel_id: row
            .try_get("channel_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        kind: ChannelSessionKind::parse(&kind)?,
        hermes_session_id: row
            .try_get("hermes_session_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        hermes_response_id: row
            .try_get("hermes_response_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        hermes_run_id: row
            .try_get("hermes_run_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        title: row
            .try_get("title")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
        updated_at: row
            .try_get::<i64, _>("updated_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
    })
}

fn row_to_message(row: &sqlx::postgres::PgRow) -> Result<ChannelMessage, ChannelStoreError> {
    let role = row
        .try_get::<String, _>("role")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;

    Ok(ChannelMessage {
        id: row
            .try_get("id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        session_id: row
            .try_get("session_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        role: ChannelMessageRole::parse(&role)?,
        content: row
            .try_get("content")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        attachments: row
            .try_get("attachments")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
    })
}

fn row_to_attachment(row: &sqlx::postgres::PgRow) -> Result<ChannelAttachment, ChannelStoreError> {
    let direction = row
        .try_get::<String, _>("direction")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;
    let kind = row
        .try_get::<String, _>("kind")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;

    Ok(ChannelAttachment {
        id: row
            .try_get("id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        session_id: row
            .try_get("session_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        message_id: row
            .try_get("message_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        direction: ChannelAttachmentDirection::parse(&direction)?,
        bucket: row
            .try_get("bucket")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        object_key: row
            .try_get("object_key")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        name: row
            .try_get("name")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        content_type: row
            .try_get("content_type")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        size: row
            .try_get::<i64, _>("size")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
        kind: ChannelAttachmentKind::parse(&kind)?,
        download_url: String::new(),
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
    }
    .with_download_url())
}

impl ChannelAttachment {
    fn with_download_url(mut self) -> Self {
        self.download_url = format!("/api/attachments/{}/download", self.id);
        self
    }
}

fn bind_memory_attachments(
    inner: &mut ChannelStoreInner,
    session_id: &str,
    message_id: &str,
    attachments: &Value,
) {
    for attachment_id in attachment_ids(attachments) {
        if let Some(attachment) = inner.attachments_by_id.get_mut(&attachment_id) {
            if attachment.session_id == session_id {
                attachment.message_id = Some(message_id.to_string());
            }
        }
    }
}

async fn bind_postgres_attachments(
    pool: &PgPool,
    session_id: &str,
    message_id: &str,
    attachments: &Value,
) -> Result<(), ChannelStoreError> {
    for attachment_id in attachment_ids(attachments) {
        sqlx::query(
            r#"
            update channel_attachments
            set message_id = $1::uuid
            where id = $2::uuid and session_id = $3::uuid
            "#,
        )
        .bind(message_id)
        .bind(attachment_id)
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;
    }
    Ok(())
}

fn attachment_ids(attachments: &Value) -> Vec<String> {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|attachment| attachment.get("id"))
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}
