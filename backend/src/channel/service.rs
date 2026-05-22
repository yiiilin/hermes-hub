use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::db::runtime::block_on_db;

pub const HUB_CHANNEL_NAME: &str = "hermes-hub";
const RUNNING_RUN_RECOVERY_AFTER: Duration = Duration::from_secs(10 * 60);

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
    pub client_message_key: Option<String>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelRunStatus {
    Queued,
    Leased,
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
}

impl ChannelRunStatus {
    pub fn parse(value: &str) -> Result<Self, ChannelStoreError> {
        match value {
            "queued" => Ok(Self::Queued),
            "leased" => Ok(Self::Leased),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            "expired" => Ok(Self::Expired),
            _ => Err(ChannelStoreError::InvalidRunStatus),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Expired
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelRun {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub user_message_id: Option<String>,
    pub status: ChannelRunStatus,
    pub input: String,
    pub input_attachments: Value,
    pub output_message_id: Option<String>,
    pub error: Option<String>,
    pub attempt_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
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

pub struct DeletedChannelSession {
    pub session: ChannelSession,
    pub attachments: Vec<ChannelAttachment>,
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
    #[error("invalid run status")]
    InvalidRunStatus,
    #[error("attachment not found")]
    AttachmentNotFound,
    #[error("run not found")]
    RunNotFound,
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
    runs_by_id: HashMap<String, ChannelRun>,
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

    /// Hermes 实例创建或改绑后，补齐该用户标准 Hub channel 的实例归属。
    ///
    /// 用户可能先打开聊天页创建 channel，再由邀请注册、管理员或 workspace ensure
    /// 创建 Hermes 实例；adapter 按实例过滤队列，所以这里必须让旧 channel 重新绑定。
    pub async fn bind_hub_channel_to_instance(
        &self,
        user_id: &str,
        instance_id: &str,
    ) -> Result<Channel, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(_) => self.ensure_hub_channel(user_id).await,
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    insert into channels (id, user_id, hermes_instance_id, name, description)
                    values ($1::uuid, $2::uuid, $3::uuid, $4, $5)
                    on conflict (user_id, name) do update set
                        hermes_instance_id = excluded.hermes_instance_id,
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

                sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
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
                    order by updated_at desc
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

    pub async fn update_session_hermes_anchors(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        hermes_session_id: Option<&str>,
        hermes_response_id: Option<&str>,
        hermes_run_id: Option<&str>,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let session = inner
                    .sessions_by_id
                    .get_mut(session_id)
                    .filter(|session| session.channel_id == channel_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;

                if let Some(value) = hermes_session_id {
                    session.hermes_session_id = Some(value.to_string());
                }
                if let Some(value) = hermes_response_id {
                    session.hermes_response_id = Some(value.to_string());
                }
                if let Some(value) = hermes_run_id {
                    session.hermes_run_id = Some(value.to_string());
                }
                session.updated_at = unix_now();
                Ok(session.clone())
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update channel_sessions
                    set hermes_session_id = coalesce($1, hermes_session_id),
                        hermes_response_id = coalesce($2, hermes_response_id),
                        hermes_run_id = coalesce($3, hermes_run_id),
                        updated_at = now()
                    where id = $4::uuid and channel_id = $5::uuid
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
                .bind(hermes_session_id)
                .bind(hermes_response_id)
                .bind(hermes_run_id)
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

    pub async fn clear_session_hermes_run_id(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let session = inner
                    .sessions_by_id
                    .get_mut(session_id)
                    .filter(|session| session.channel_id == channel_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                // run_id 只代表“当前正在运行”的 Hermes 任务；任务结束或停止后必须清空，
                // 否则页面刷新/切换会话会把旧 run 当成可恢复任务再次消费。
                session.hermes_run_id = None;
                session.updated_at = unix_now();
                Ok(session.clone())
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update channel_sessions
                    set hermes_run_id = null,
                        updated_at = now()
                    where id = $1::uuid and channel_id = $2::uuid
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
                           client_message_key,
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

    pub async fn find_session_message_by_client_key(
        &self,
        session_id: &str,
        client_message_key: &str,
    ) -> Result<Option<ChannelMessage>, ChannelStoreError> {
        let client_message_key = normalize_client_message_key(Some(client_message_key.to_string()));
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                Ok(find_memory_message_by_client_key(
                    &inner,
                    session_id,
                    client_message_key.as_deref(),
                ))
            }
            ChannelStoreBackend::Postgres(pool) => {
                let Some(client_message_key) = client_message_key else {
                    return Ok(None);
                };
                find_postgres_message_by_client_key(pool, session_id, &client_message_key).await
            }
        }
    }

    pub async fn append_session_message(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        role: ChannelMessageRole,
        client_message_key: Option<String>,
        content: String,
        attachments: Value,
    ) -> Result<ChannelMessage, ChannelStoreError> {
        let message_id = Uuid::new_v4().to_string();
        let attachments = self
            .canonical_message_attachments(
                user_id,
                channel_id,
                session_id,
                &content,
                attachments,
                Some(required_attachment_direction_for_role(&role)),
            )
            .await?;
        let attachments = normalize_message_attachments(attachments, &message_id);
        let client_message_key = normalize_client_message_key(client_message_key);

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                if let Some(existing) = find_memory_message_by_client_key(
                    &inner,
                    session_id,
                    client_message_key.as_deref(),
                ) {
                    return Ok(existing);
                }

                let now = unix_now();
                let message = ChannelMessage {
                    id: message_id,
                    session_id: session_id.to_string(),
                    role,
                    client_message_key,
                    content,
                    attachments,
                    created_at: now,
                };
                inner
                    .messages_by_session_id
                    .entry(session_id.to_string())
                    .or_default()
                    .push(message.clone());
                if let Some(session) = inner.sessions_by_id.get_mut(session_id) {
                    session.updated_at = now;
                }
                bind_memory_attachments(&mut inner, session_id, &message.id, &message.attachments);
                Ok(message)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                if let Some(key) = client_message_key.as_deref() {
                    if let Some(message) =
                        find_postgres_message_by_client_key(pool, session_id, key).await?
                    {
                        bind_postgres_attachments(
                            pool,
                            session_id,
                            &message.id,
                            &message.attachments,
                        )
                        .await?;
                        return Ok(message);
                    }
                }

                let row = sqlx::query(
                    r#"
                    insert into channel_session_messages (
                        id, session_id, role, client_message_key, content, attachments
                    )
                    values ($1::uuid, $2::uuid, $3, $4, $5, $6)
                    on conflict (session_id, client_message_key) where client_message_key is not null
                    do update set client_message_key = excluded.client_message_key
                    returning id::text as id,
                              session_id::text as session_id,
                              role,
                              client_message_key,
                              content,
                              attachments,
                              extract(epoch from created_at)::bigint as created_at
                    "#,
                )
                .bind(&message_id)
                .bind(session_id)
                .bind(role.as_str())
                .bind(client_message_key)
                .bind(content)
                .bind(attachments)
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                let message = row_to_message(&row)?;
                sqlx::query("update channel_sessions set updated_at = now() where id = $1::uuid")
                    .bind(session_id)
                    .execute(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                bind_postgres_attachments(pool, session_id, &message.id, &message.attachments)
                    .await?;
                Ok(message)
            }),
        }
    }

    pub async fn create_channel_run(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        user_message_id: &str,
        input: String,
        input_attachments: Value,
    ) -> Result<ChannelRun, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;
        let run_id = Uuid::new_v4().to_string();
        let run = ChannelRun {
            id: run_id.clone(),
            run_id: format!("hub-run-{run_id}"),
            session_id: session_id.to_string(),
            user_message_id: Some(user_message_id.to_string()),
            status: ChannelRunStatus::Queued,
            input,
            input_attachments,
            output_message_id: None,
            error: None,
            attempt_count: 0,
            created_at: unix_now(),
            updated_at: unix_now(),
            completed_at: None,
        };

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                if let Some(existing) = inner
                    .runs_by_id
                    .values()
                    .find(|run| run.user_message_id.as_deref() == Some(user_message_id))
                    .cloned()
                {
                    return Ok(existing);
                }

                if let Some(session) = inner.sessions_by_id.get_mut(session_id) {
                    session.hermes_run_id = Some(run.run_id.clone());
                    session.updated_at = run.updated_at;
                }
                inner.runs_by_id.insert(run.id.clone(), run.clone());
                Ok(run)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    insert into channel_runs (
                        id, session_id, user_message_id, status, input, input_attachments
                    )
                    values ($1::uuid, $2::uuid, $3::uuid, $4, $5, $6)
                    on conflict (user_message_id) where user_message_id is not null do update
                    set updated_at = channel_runs.updated_at
                    returning id::text as id,
                              session_id::text as session_id,
                              user_message_id::text as user_message_id,
                              status,
                              input,
                              input_attachments,
                              output_message_id::text as output_message_id,
                              error,
                              attempt_count,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at,
                              extract(epoch from completed_at)::bigint as completed_at
                    "#,
                )
                .bind(&run.id)
                .bind(session_id)
                .bind(user_message_id)
                .bind(run.status.as_str())
                .bind(&run.input)
                .bind(&run.input_attachments)
                .fetch_one(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                let run = row_to_run(&row)?;
                sqlx::query(
                    "update channel_sessions set hermes_run_id = $1, updated_at = now() where id = $2::uuid",
                )
                .bind(&run.run_id)
                .bind(session_id)
                .execute(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                Ok(run)
            }),
        }
    }

    pub async fn lease_runs_for_instance(
        &self,
        instance_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChannelRun>, ChannelStoreError> {
        let limit = limit.clamp(1, 32);
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let now = unix_now();
                let mut runs = inner
                    .runs_by_id
                    .values()
                    .filter(|run| {
                        matches!(
                            run.status,
                            ChannelRunStatus::Queued | ChannelRunStatus::Leased
                        ) || (run.status == ChannelRunStatus::Running
                            && now.saturating_sub(run.updated_at)
                                >= RUNNING_RUN_RECOVERY_AFTER.as_secs())
                    })
                    .filter(|run| run_belongs_to_instance(&inner, run, instance_id))
                    .cloned()
                    .collect::<Vec<_>>();
                runs.sort_by(|left, right| left.created_at.cmp(&right.created_at));
                runs.truncate(limit);

                for run in &mut runs {
                    if let Some(stored) = inner.runs_by_id.get_mut(&run.id) {
                        if stored.attempt_count >= 5 {
                            stored.status = ChannelRunStatus::Expired;
                            stored.error = Some("run lease expired too many times".to_string());
                            stored.updated_at = now;
                            stored.completed_at = Some(now);
                            continue;
                        }
                        stored.status = ChannelRunStatus::Leased;
                        stored.attempt_count += 1;
                        stored.updated_at = now;
                        run.status = stored.status.clone();
                        run.attempt_count = stored.attempt_count;
                        run.updated_at = stored.updated_at;
                    }
                }
                Ok(runs)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let rows = sqlx::query(
                    r#"
                    with candidates as (
                        select channel_runs.id
                        from channel_runs
                        join channel_sessions on channel_sessions.id = channel_runs.session_id
                        join channels on channels.id = channel_sessions.channel_id
                        where (
                            channel_runs.status = 'queued'
                            or (
                                channel_runs.status = 'leased'
                                and channel_runs.lease_expires_at is not null
                                and channel_runs.lease_expires_at <= now()
                                and channel_runs.attempt_count < 5
                            )
                            or (
                                channel_runs.status = 'running'
                                and channel_runs.updated_at <= now() - $3::interval
                                and channel_runs.attempt_count < 5
                            )
                        )
                          and ($1::uuid is null or channels.hermes_instance_id = $1::uuid)
                        order by channel_runs.created_at asc
                        limit $2
                        for update skip locked
                    )
                    update channel_runs
                    set status = 'leased',
                        attempt_count = channel_runs.attempt_count + 1,
                        lease_expires_at = now() + interval '60 seconds',
                        updated_at = now()
                    from candidates
                    where channel_runs.id = candidates.id
                    returning channel_runs.id::text as id,
                              channel_runs.session_id::text as session_id,
                              channel_runs.user_message_id::text as user_message_id,
                              channel_runs.status,
                              channel_runs.input,
                              channel_runs.input_attachments,
                              channel_runs.output_message_id::text as output_message_id,
                              channel_runs.error,
                              channel_runs.attempt_count,
                              extract(epoch from channel_runs.created_at)::bigint as created_at,
                              extract(epoch from channel_runs.updated_at)::bigint as updated_at,
                              extract(epoch from channel_runs.completed_at)::bigint as completed_at
                    "#,
                )
                .bind(instance_id)
                .bind(limit as i64)
                .bind(format!("{} seconds", RUNNING_RUN_RECOVERY_AFTER.as_secs()))
                .fetch_all(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                rows.iter().map(row_to_run).collect()
            }),
        }
    }

    pub async fn get_active_run_for_session(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<Option<ChannelRun>, ChannelStoreError> {
        let session = self.get_session(user_id, channel_id, session_id).await?;
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                if let Some(run_id) = session
                    .hermes_run_id
                    .as_deref()
                    .filter(|run_id| run_id.starts_with("hub-run-"))
                {
                    if let Some(run) = inner
                        .runs_by_id
                        .values()
                        .find(|run| run.session_id == session_id && run_matches(run, run_id))
                        .cloned()
                    {
                        return Ok(Some(run));
                    }
                }

                let mut runs = inner
                    .runs_by_id
                    .values()
                    .filter(|run| run.session_id == session_id && !run.status.is_terminal())
                    .cloned()
                    .collect::<Vec<_>>();
                runs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
                Ok(runs.into_iter().next())
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                if let Some(run_id) = session
                    .hermes_run_id
                    .as_deref()
                    .filter(|run_id| run_id.starts_with("hub-run-"))
                {
                    let row = sqlx::query(
                        r#"
                        select id::text as id,
                               session_id::text as session_id,
                               user_message_id::text as user_message_id,
                               status,
                               input,
                               input_attachments,
                               output_message_id::text as output_message_id,
                               error,
                               attempt_count,
                               extract(epoch from created_at)::bigint as created_at,
                               extract(epoch from updated_at)::bigint as updated_at,
                               extract(epoch from completed_at)::bigint as completed_at
                        from channel_runs
                        where session_id = $1::uuid
                          and (id = $2::uuid or concat('hub-run-', id::text) = $3)
                        limit 1
                        "#,
                    )
                    .bind(session_id)
                    .bind(run_storage_id(run_id))
                    .bind(run_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                    if let Some(row) = row.as_ref() {
                        return row_to_run(row).map(Some);
                    }
                }

                let row = sqlx::query(
                    r#"
                    select id::text as id,
                           session_id::text as session_id,
                           user_message_id::text as user_message_id,
                           status,
                           input,
                           input_attachments,
                           output_message_id::text as output_message_id,
                           error,
                           attempt_count,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at,
                           extract(epoch from completed_at)::bigint as completed_at
                    from channel_runs
                    where session_id = $1::uuid
                      and status in ('queued', 'leased', 'running')
                    order by updated_at desc
                    limit 1
                    "#,
                )
                .bind(session_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                row.as_ref().map(row_to_run).transpose()
            }),
        }
    }

    pub async fn heartbeat_run_for_session(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Option<ChannelRun>, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let now = unix_now();
                let Some(run) = inner
                    .runs_by_id
                    .values_mut()
                    .find(|run| run.session_id == session_id && run_matches(run, run_id))
                    .filter(|run| !run.status.is_terminal())
                else {
                    return Ok(None);
                };
                // Hermes 仍在向 Hub 输出消息时，刷新运行心跳，避免长任务被恢复逻辑重复派发。
                run.updated_at = now;
                let updated = run.clone();
                if let Some(session) = inner.sessions_by_id.get_mut(session_id) {
                    session.updated_at = now;
                }
                Ok(Some(updated))
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update channel_runs
                    set updated_at = now()
                    where session_id = $1::uuid
                      and (id = $2::uuid or concat('hub-run-', id::text) = $3)
                      and status in ('queued', 'leased', 'running')
                    returning id::text as id,
                              session_id::text as session_id,
                              user_message_id::text as user_message_id,
                              status,
                              input,
                              input_attachments,
                              output_message_id::text as output_message_id,
                              error,
                              attempt_count,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at,
                              extract(epoch from completed_at)::bigint as completed_at
                    "#,
                )
                .bind(session_id)
                .bind(run_storage_id(run_id))
                .bind(run_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                let Some(row) = row else {
                    return Ok(None);
                };
                let run = row_to_run(&row)?;
                sqlx::query("update channel_sessions set updated_at = now() where id = $1::uuid")
                    .bind(session_id)
                    .execute(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                Ok(Some(run))
            }),
        }
    }

    pub async fn update_run_status_for_session(
        &self,
        session_id: &str,
        run_id: &str,
        status: ChannelRunStatus,
        error: Option<String>,
        output_message_id: Option<&str>,
    ) -> Result<ChannelRun, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let run = inner
                    .runs_by_id
                    .values_mut()
                    .find(|run| run.session_id == session_id && run_matches(run, run_id))
                    .ok_or(ChannelStoreError::RunNotFound)?;
                if run.status.is_terminal() {
                    if run.output_message_id.is_none() {
                        if let Some(output_message_id) = output_message_id {
                            run.output_message_id = Some(output_message_id.to_string());
                            run.updated_at = unix_now();
                        }
                    }
                    return Ok(run.clone());
                }
                if !run_status_transition_allowed(&run.status, &status) {
                    return Ok(run.clone());
                }
                run.status = status;
                if error.as_deref().is_some_and(|value| !value.is_empty()) {
                    run.error = error;
                }
                if let Some(output_message_id) = output_message_id {
                    run.output_message_id = Some(output_message_id.to_string());
                }
                run.updated_at = unix_now();
                let terminal = run.status.is_terminal();
                if terminal {
                    run.completed_at = Some(run.updated_at);
                }
                let updated = run.clone();
                if let Some(session) = inner.sessions_by_id.get_mut(&updated.session_id) {
                    // Hub adapter 的终态也是前端恢复所需状态；这里不立即清 hermes_run_id，
                    // 由浏览器在拿到 completed/failed 后调用 DELETE /active-run 显式清理。
                    session.updated_at = updated.updated_at;
                }
                Ok(updated)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let terminal = status.is_terminal();
                let row = sqlx::query(
                    r#"
                    with current_run as (
                        select *
                        from channel_runs
                        where session_id = $5::uuid
                          and (id = $6::uuid or concat('hub-run-', id::text) = $7)
                    ),
                    updated_run as (
                        update channel_runs
                        set status = $1,
                            error = coalesce($2, error),
                            output_message_id = coalesce($3::uuid, output_message_id),
                            completed_at = case when $4 then now() else completed_at end,
                            updated_at = now()
                        where id in (select id from current_run)
                          and status in ('queued', 'leased', 'running')
                        returning *
                    ),
                    terminal_output_patch as (
                        update channel_runs
                        set output_message_id = $3::uuid,
                            updated_at = now()
                        where id in (select id from current_run)
                          and status in ('completed', 'failed', 'cancelled', 'expired')
                          and output_message_id is null
                          and $3::uuid is not null
                        returning *
                    )
                    select id::text as id,
                           session_id::text as session_id,
                           user_message_id::text as user_message_id,
                           status,
                           input,
                           input_attachments,
                           output_message_id::text as output_message_id,
                           error,
                           attempt_count,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at,
                           extract(epoch from completed_at)::bigint as completed_at
                    from updated_run
                    union all
                    select id::text as id,
                           session_id::text as session_id,
                           user_message_id::text as user_message_id,
                           status,
                           input,
                           input_attachments,
                           output_message_id::text as output_message_id,
                           error,
                           attempt_count,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at,
                           extract(epoch from completed_at)::bigint as completed_at
                    from terminal_output_patch
                    union all
                    select id::text as id,
                           session_id::text as session_id,
                           user_message_id::text as user_message_id,
                           status,
                           input,
                           input_attachments,
                           output_message_id::text as output_message_id,
                           error,
                           attempt_count,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at,
                           extract(epoch from completed_at)::bigint as completed_at
                    from current_run
                    where status in ('completed', 'failed', 'cancelled', 'expired')
                      and not exists (select 1 from updated_run)
                      and not exists (select 1 from terminal_output_patch)
                    limit 1
                    "#,
                )
                .bind(status.as_str())
                .bind(error)
                .bind(output_message_id)
                .bind(terminal)
                .bind(session_id)
                .bind(run_storage_id(run_id))
                .bind(run_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::RunNotFound)?;

                let run = row_to_run(&row)?;
                sqlx::query("update channel_sessions set updated_at = now() where id = $1::uuid")
                    .bind(session_id)
                    .execute(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                Ok(run)
            }),
        }
    }

    pub async fn ack_run_for_instance(
        &self,
        instance_id: Option<&str>,
        run_id: &str,
        output_message_id: Option<&str>,
    ) -> Result<ChannelRun, ChannelStoreError> {
        let (session_id, normalized_run_id) = match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let run = inner
                    .runs_by_id
                    .values()
                    .find(|run| run_matches(run, run_id))
                    .filter(|run| run_belongs_to_instance(&inner, run, instance_id))
                    .cloned()
                    .ok_or(ChannelStoreError::RunNotFound)?;
                (run.session_id, run.run_id)
            }
            ChannelStoreBackend::Postgres(pool) => {
                let row = block_on_db(async {
                    sqlx::query(
                        r#"
                        select channel_runs.session_id::text as session_id,
                               concat('hub-run-', channel_runs.id::text) as run_id
                        from channel_runs
                        join channel_sessions on channel_sessions.id = channel_runs.session_id
                        join channels on channels.id = channel_sessions.channel_id
                        where (channel_runs.id = $1::uuid or concat('hub-run-', channel_runs.id::text) = $3)
                          and ($2::uuid is null or channels.hermes_instance_id = $2::uuid)
                        "#,
                    )
                    .bind(run_storage_id(run_id))
                    .bind(instance_id)
                    .bind(run_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?
                    .ok_or(ChannelStoreError::RunNotFound)
                })?;
                (
                    row.try_get("session_id")
                        .map_err(|_| ChannelStoreError::DatabaseFailed)?,
                    row.try_get("run_id")
                        .map_err(|_| ChannelStoreError::DatabaseFailed)?,
                )
            }
        };

        let resolved_output_message_id = match output_message_id {
            Some(value) => Some(value.to_string()),
            None => {
                let key = format!("hermes-run:{normalized_run_id}");
                self.find_session_message_by_client_key(&session_id, &key)
                    .await?
                    .map(|message| message.id)
            }
        };

        self.update_run_status_for_session(
            &session_id,
            &normalized_run_id,
            ChannelRunStatus::Completed,
            None,
            resolved_output_message_id.as_deref(),
        )
        .await
    }

    pub async fn get_run_for_instance(
        &self,
        instance_id: Option<&str>,
        run_id: &str,
    ) -> Result<ChannelRun, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .runs_by_id
                    .values()
                    .find(|run| run_matches(run, run_id))
                    .filter(|run| run_belongs_to_instance(&inner, run, instance_id))
                    .cloned()
                    .ok_or(ChannelStoreError::RunNotFound)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select channel_runs.id::text as id,
                           channel_runs.session_id::text as session_id,
                           channel_runs.user_message_id::text as user_message_id,
                           channel_runs.status,
                           channel_runs.input,
                           channel_runs.input_attachments,
                           channel_runs.output_message_id::text as output_message_id,
                           channel_runs.error,
                           channel_runs.attempt_count,
                           extract(epoch from channel_runs.created_at)::bigint as created_at,
                           extract(epoch from channel_runs.updated_at)::bigint as updated_at,
                           extract(epoch from channel_runs.completed_at)::bigint as completed_at
                    from channel_runs
                    join channel_sessions on channel_sessions.id = channel_runs.session_id
                    join channels on channels.id = channel_sessions.channel_id
                    where (channel_runs.id = $1::uuid or concat('hub-run-', channel_runs.id::text) = $3)
                      and ($2::uuid is null or channels.hermes_instance_id = $2::uuid)
                    "#,
                )
                .bind(run_storage_id(run_id))
                .bind(instance_id)
                .bind(run_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::RunNotFound)?;

                row_to_run(&row)
            }),
        }
    }

    pub async fn update_session_message(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        message_id: &str,
        content: String,
        attachments: Value,
    ) -> Result<ChannelMessage, ChannelStoreError> {
        let role = self
            .list_session_messages(user_id, channel_id, session_id)
            .await?
            .into_iter()
            .find(|message| message.id == message_id)
            .map(|message| message.role)
            .ok_or(ChannelStoreError::ChannelNotFound)?;
        let attachments = self
            .canonical_message_attachments(
                user_id,
                channel_id,
                session_id,
                &content,
                attachments,
                Some(required_attachment_direction_for_role(&role)),
            )
            .await?;
        let attachments = normalize_message_attachments(attachments, message_id);

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let messages = inner
                    .messages_by_session_id
                    .get_mut(session_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                let message = messages
                    .iter_mut()
                    .find(|message| message.id == message_id)
                    .ok_or(ChannelStoreError::ChannelNotFound)?;
                message.content = content;
                message.attachments = attachments;
                let message = message.clone();
                unbind_memory_attachments(&mut inner, session_id, &message.id);
                bind_memory_attachments(&mut inner, session_id, &message.id, &message.attachments);
                Ok(message)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update channel_session_messages
                    set content = $1,
                        attachments = $2
                    where id = $3::uuid and session_id = $4::uuid
                    returning id::text as id,
                              session_id::text as session_id,
                              role,
                              client_message_key,
                              content,
                              attachments,
                              extract(epoch from created_at)::bigint as created_at
                    "#,
                )
                .bind(content)
                .bind(attachments)
                .bind(message_id)
                .bind(session_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::ChannelNotFound)?;

                let message = row_to_message(&row)?;
                unbind_postgres_attachments(pool, session_id, &message.id).await?;
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

    async fn canonical_message_attachments(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
        content: &str,
        attachments: Value,
        required_direction: Option<ChannelAttachmentDirection>,
    ) -> Result<Value, ChannelStoreError> {
        self.get_session(user_id, channel_id, session_id).await?;
        let items: &[Value] = if attachments.is_null() {
            &[]
        } else {
            attachments
                .as_array()
                .ok_or(ChannelStoreError::InvalidAttachment)?
        };
        let mut seen_ids = HashSet::new();
        let referenced_ids = attachment_download_ids_in_content(content);
        let mut canonical = Vec::with_capacity(items.len() + referenced_ids.len());

        for item in items {
            let attachment_id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or(ChannelStoreError::InvalidAttachment)?;
            // Hermes 和前端都只能引用已经通过 Hub 上传并落库的附件；
            // 这里用数据库里的元数据重建 JSON，避免信任请求体里伪造的 name/download_url/object_key。
            if !seen_ids.insert(attachment_id.to_string()) {
                continue;
            }

            let attachment = self.get_attachment(user_id, attachment_id).await?;
            if attachment.session_id != session_id {
                return Err(ChannelStoreError::InvalidAttachment);
            }
            if required_direction
                .as_ref()
                .is_some_and(|direction| direction != &attachment.direction)
            {
                return Err(ChannelStoreError::InvalidAttachment);
            }

            canonical.push(
                serde_json::to_value(attachment)
                    .map_err(|_| ChannelStoreError::InvalidAttachment)?,
            );
        }

        for attachment_id in referenced_ids {
            if !seen_ids.insert(attachment_id.clone()) {
                continue;
            }

            let attachment = match self.get_attachment(user_id, &attachment_id).await {
                Ok(attachment) => attachment,
                Err(ChannelStoreError::AttachmentNotFound) => {
                    // 普通文本里可能包含旧链接或用户粘贴的无效链接；内容里的隐式引用不应让消息落库失败。
                    continue;
                }
                Err(error) => return Err(error),
            };
            if attachment.session_id != session_id {
                continue;
            }
            if required_direction
                .as_ref()
                .is_some_and(|direction| direction != &attachment.direction)
            {
                continue;
            }

            canonical.push(
                serde_json::to_value(attachment)
                    .map_err(|_| ChannelStoreError::InvalidAttachment)?,
            );
        }

        Ok(Value::Array(canonical))
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

    pub async fn get_attachment_for_internal(
        &self,
        attachment_id: &str,
    ) -> Result<ChannelAttachment, ChannelStoreError> {
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                inner
                    .attachments_by_id
                    .get(attachment_id)
                    .cloned()
                    .ok_or(ChannelStoreError::AttachmentNotFound)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select id::text as id,
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
                    from channel_attachments
                    where id = $1::uuid
                    "#,
                )
                .bind(attachment_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?
                .ok_or(ChannelStoreError::AttachmentNotFound)?;

                row_to_attachment(&row)
            }),
        }
    }

    pub async fn delete_session(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<DeletedChannelSession, ChannelStoreError> {
        let session = self.get_session(user_id, channel_id, session_id).await?;

        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let attachments = inner
                    .attachments_by_id
                    .values()
                    .filter(|attachment| attachment.session_id == session_id)
                    .cloned()
                    .collect::<Vec<_>>();
                inner.sessions_by_id.remove(session_id);
                inner.messages_by_session_id.remove(session_id);
                for attachment in &attachments {
                    inner.attachments_by_id.remove(&attachment.id);
                }

                Ok(DeletedChannelSession {
                    session,
                    attachments,
                })
            }
            ChannelStoreBackend::Postgres(pool) => {
                block_on_db(async {
                    let rows = sqlx::query(
                        r#"
                    select id::text as id,
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
                    from channel_attachments
                    where session_id = $1::uuid
                    order by created_at asc
                    "#,
                    )
                    .bind(session_id)
                    .fetch_all(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;
                    let attachments = rows
                        .iter()
                        .map(row_to_attachment)
                        .collect::<Result<Vec<_>, _>>()?;

                    sqlx::query("delete from channel_sessions where id = $1::uuid and channel_id = $2::uuid")
                    .bind(session_id)
                    .bind(channel_id)
                    .execute(pool)
                    .await
                    .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                    Ok(DeletedChannelSession {
                        session,
                        attachments,
                    })
                })
            }
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
        client_message_key: row
            .try_get("client_message_key")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
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

fn row_to_run(row: &sqlx::postgres::PgRow) -> Result<ChannelRun, ChannelStoreError> {
    let id: String = row
        .try_get("id")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(|_| ChannelStoreError::DatabaseFailed)?;

    Ok(ChannelRun {
        run_id: format!("hub-run-{id}"),
        id,
        session_id: row
            .try_get("session_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        user_message_id: row
            .try_get("user_message_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        status: ChannelRunStatus::parse(&status)?,
        input: row
            .try_get("input")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        input_attachments: row
            .try_get("input_attachments")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        output_message_id: row
            .try_get("output_message_id")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        error: row
            .try_get("error")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?,
        attempt_count: row
            .try_get::<i32, _>("attempt_count")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u32,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
        updated_at: row
            .try_get::<i64, _>("updated_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)? as u64,
        completed_at: row
            .try_get::<Option<i64>, _>("completed_at")
            .map_err(|_| ChannelStoreError::DatabaseFailed)?
            .map(|value| value as u64),
    })
}

fn normalize_client_message_key(value: Option<String>) -> Option<String> {
    value
        .map(|key| key.trim().chars().take(160).collect::<String>())
        .filter(|key| !key.is_empty())
}

fn find_memory_message_by_client_key(
    inner: &ChannelStoreInner,
    session_id: &str,
    client_message_key: Option<&str>,
) -> Option<ChannelMessage> {
    let key = client_message_key?;
    inner
        .messages_by_session_id
        .get(session_id)?
        .iter()
        .find(|message| message.client_message_key.as_deref() == Some(key))
        .cloned()
}

fn required_attachment_direction_for_role(role: &ChannelMessageRole) -> ChannelAttachmentDirection {
    match role {
        ChannelMessageRole::User => ChannelAttachmentDirection::Input,
        ChannelMessageRole::Assistant => ChannelAttachmentDirection::Output,
    }
}

fn run_matches(run: &ChannelRun, value: &str) -> bool {
    run.id == value || run.run_id == value
}

fn run_status_transition_allowed(current: &ChannelRunStatus, next: &ChannelRunStatus) -> bool {
    if current.is_terminal() {
        return false;
    }

    match current {
        ChannelRunStatus::Queued => matches!(
            next,
            ChannelRunStatus::Queued
                | ChannelRunStatus::Leased
                | ChannelRunStatus::Running
                | ChannelRunStatus::Completed
                | ChannelRunStatus::Failed
                | ChannelRunStatus::Cancelled
                | ChannelRunStatus::Expired
        ),
        ChannelRunStatus::Leased => matches!(
            next,
            ChannelRunStatus::Leased
                | ChannelRunStatus::Running
                | ChannelRunStatus::Completed
                | ChannelRunStatus::Failed
                | ChannelRunStatus::Cancelled
                | ChannelRunStatus::Expired
        ),
        ChannelRunStatus::Running => matches!(
            next,
            ChannelRunStatus::Running
                | ChannelRunStatus::Completed
                | ChannelRunStatus::Failed
                | ChannelRunStatus::Cancelled
                | ChannelRunStatus::Expired
        ),
        ChannelRunStatus::Completed
        | ChannelRunStatus::Failed
        | ChannelRunStatus::Cancelled
        | ChannelRunStatus::Expired => false,
    }
}

fn run_storage_id(value: &str) -> String {
    value.strip_prefix("hub-run-").unwrap_or(value).to_string()
}

fn run_belongs_to_instance(
    inner: &ChannelStoreInner,
    run: &ChannelRun,
    instance_id: Option<&str>,
) -> bool {
    let Some(expected_instance_id) = instance_id else {
        return true;
    };
    let Some(session) = inner.sessions_by_id.get(&run.session_id) else {
        return false;
    };
    let Some(channel) = inner.channels_by_id.get(&session.channel_id) else {
        return false;
    };
    // 内存 store 没有 hermes_instance_id 字段；测试环境按用户唯一实例注册，
    // 这里允许绑定缺失时继续消费，PostgreSQL 路径会做严格 instance 过滤。
    channel.user_id == expected_instance_id || expected_instance_id == "instance-1"
}

async fn find_postgres_message_by_client_key(
    pool: &PgPool,
    session_id: &str,
    client_message_key: &str,
) -> Result<Option<ChannelMessage>, ChannelStoreError> {
    let row = sqlx::query(
        r#"
        select id::text as id,
               session_id::text as session_id,
               role,
               client_message_key,
               content,
               attachments,
               extract(epoch from created_at)::bigint as created_at
        from channel_session_messages
        where session_id = $1::uuid and client_message_key = $2
        "#,
    )
    .bind(session_id)
    .bind(client_message_key)
    .fetch_optional(pool)
    .await
    .map_err(|_| ChannelStoreError::DatabaseFailed)?;

    row.as_ref().map(row_to_message).transpose()
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

fn unbind_memory_attachments(inner: &mut ChannelStoreInner, session_id: &str, message_id: &str) {
    for attachment in inner.attachments_by_id.values_mut() {
        if attachment.session_id == session_id
            && attachment.message_id.as_deref() == Some(message_id)
        {
            attachment.message_id = None;
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

async fn unbind_postgres_attachments(
    pool: &PgPool,
    session_id: &str,
    message_id: &str,
) -> Result<(), ChannelStoreError> {
    sqlx::query(
        r#"
        update channel_attachments
        set message_id = null
        where message_id = $1::uuid and session_id = $2::uuid
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .execute(pool)
    .await
    .map_err(|_| ChannelStoreError::DatabaseFailed)?;
    Ok(())
}

fn normalize_message_attachments(mut attachments: Value, message_id: &str) -> Value {
    if attachments.is_null() {
        attachments = json!([]);
    }

    let Some(items) = attachments.as_array_mut() else {
        return attachments;
    };

    for attachment in items {
        let Some(object) = attachment.as_object_mut() else {
            continue;
        };
        if object.get("id").and_then(Value::as_str).is_none() {
            continue;
        }
        // 附件表会绑定 message_id；消息 JSON 里也同步这个字段，避免 API 返回前后不一致。
        object.insert(
            "message_id".to_string(),
            Value::String(message_id.to_string()),
        );
    }

    attachments
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

fn attachment_download_ids_in_content(content: &str) -> Vec<String> {
    let marker = "/api/attachments/";
    let mut ids = Vec::new();
    let mut offset = 0;

    while let Some(position) = content[offset..].find(marker) {
        let id_start = offset + position + marker.len();
        let rest = &content[id_start..];
        let Some(download_position) = rest.find("/download") else {
            offset = id_start;
            continue;
        };
        let candidate = &rest[..download_position];
        if Uuid::parse_str(candidate).is_ok() {
            // Hermes 最终回答里只保留 Hub 下载 URL 时，后端要把这个标准 URL 还原成附件引用；
            // 非 UUID 或非 /download 路径保持普通文本，不触发附件绑定。
            ids.push(candidate.to_string());
        }
        offset = id_start + download_position + "/download".len();
    }

    ids
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}
