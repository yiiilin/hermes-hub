use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::db::runtime::block_on_db;

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

#[derive(Debug, Error)]
pub enum ChannelStoreError {
    #[error("channel not found")]
    ChannelNotFound,
    #[error("invalid session kind")]
    InvalidSessionKind,
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
        match &self.backend {
            ChannelStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| ChannelStoreError::LockFailed)?;
                let mut channels = inner
                    .channels_by_id
                    .values()
                    .filter(|channel| channel.user_id == user_id)
                    .cloned()
                    .collect::<Vec<_>>();

                channels.sort_by(|left, right| right.created_at.cmp(&left.created_at));
                Ok(channels)
            }
            ChannelStoreBackend::Postgres(pool) => block_on_db(async {
                let rows = sqlx::query(
                    r#"
                    select id::text as id,
                           user_id::text as user_id,
                           name,
                           description,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at
                    from channels
                    where user_id = $1::uuid
                    order by created_at desc
                    "#,
                )
                .bind(user_id)
                .fetch_all(pool)
                .await
                .map_err(|_| ChannelStoreError::DatabaseFailed)?;

                rows.iter().map(row_to_channel).collect()
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}
