use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

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
}

#[derive(Clone, Default)]
pub struct ChannelStore {
    inner: Arc<Mutex<ChannelStoreInner>>,
}

#[derive(Default)]
struct ChannelStoreInner {
    channels_by_id: HashMap<String, Channel>,
    sessions_by_id: HashMap<String, ChannelSession>,
}

impl ChannelStore {
    pub fn create_channel(
        &self,
        user_id: &str,
        name: &str,
        description: Option<String>,
    ) -> Result<Channel, ChannelStoreError> {
        let now = unix_now();
        let channel = Channel {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            name: name.trim().to_string(),
            description,
            created_at: now,
            updated_at: now,
        };

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        inner
            .channels_by_id
            .insert(channel.id.clone(), channel.clone());
        Ok(channel)
    }

    pub fn list_channels(&self, user_id: &str) -> Result<Vec<Channel>, ChannelStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        let mut channels = inner
            .channels_by_id
            .values()
            .filter(|channel| channel.user_id == user_id)
            .cloned()
            .collect::<Vec<_>>();

        channels.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(channels)
    }

    pub fn get_channel(
        &self,
        user_id: &str,
        channel_id: &str,
    ) -> Result<Channel, ChannelStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        inner
            .channels_by_id
            .get(channel_id)
            .filter(|channel| channel.user_id == user_id)
            .cloned()
            .ok_or(ChannelStoreError::ChannelNotFound)
    }

    pub fn create_session(
        &self,
        user_id: &str,
        channel_id: &str,
        kind: ChannelSessionKind,
        title: Option<String>,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_channel(user_id, channel_id)?;

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

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        inner
            .sessions_by_id
            .insert(session.id.clone(), session.clone());
        Ok(session)
    }

    pub fn list_sessions(
        &self,
        user_id: &str,
        channel_id: &str,
    ) -> Result<Vec<ChannelSession>, ChannelStoreError> {
        self.get_channel(user_id, channel_id)?;

        let inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        let mut sessions = inner
            .sessions_by_id
            .values()
            .filter(|session| session.channel_id == channel_id)
            .cloned()
            .collect::<Vec<_>>();

        sessions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(sessions)
    }

    pub fn get_session(
        &self,
        user_id: &str,
        channel_id: &str,
        session_id: &str,
    ) -> Result<ChannelSession, ChannelStoreError> {
        self.get_channel(user_id, channel_id)?;

        let inner = self
            .inner
            .lock()
            .map_err(|_| ChannelStoreError::LockFailed)?;
        inner
            .sessions_by_id
            .get(session_id)
            .filter(|session| session.channel_id == channel_id)
            .cloned()
            .ok_or(ChannelStoreError::ChannelNotFound)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}
