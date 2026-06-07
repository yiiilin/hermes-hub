use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use super::service::{ChannelMessage, ChannelRun, ChannelSession};

const CHANNEL_EVENT_CAPACITY: usize = 512;

/// 会话事件总线负责把已经落库的 Hub 消息实时广播给浏览器连接。
///
/// 它不是数据源，历史消息仍然以 PostgreSQL/内存 store 为准；SSE 只负责把
/// 新变更及时推送到当前打开该 session 的客户端。
#[derive(Clone)]
pub struct SessionEventHub {
    sender: Arc<broadcast::Sender<SessionEvent>>,
}

impl Default for SessionEventHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_EVENT_CAPACITY);
        Self {
            sender: Arc::new(sender),
        }
    }
}

impl SessionEventHub {
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: SessionEvent) {
        // 没有浏览器连接时发送失败是正常状态；消息已经持久化，不需要重试。
        let _ = self.sender.send(event);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    MessageCreated { message: ChannelMessage },
    MessageUpdated { message: ChannelMessage },
    SessionUpdated { session: ChannelSession },
    RunUpdated { run: ChannelRun },
    BusinessToolRequest { request: BusinessToolRequestEvent },
    RunCleared { session_id: String },
    SessionDeleted { session_id: String },
}

impl SessionEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            SessionEvent::MessageCreated { .. } => "message_created",
            SessionEvent::MessageUpdated { .. } => "message_updated",
            SessionEvent::SessionUpdated { .. } => "session_updated",
            SessionEvent::RunUpdated { .. } => "run_updated",
            SessionEvent::BusinessToolRequest { .. } => "business_tool_request",
            SessionEvent::RunCleared { .. } => "run_cleared",
            SessionEvent::SessionDeleted { .. } => "session_deleted",
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            SessionEvent::MessageCreated { message } | SessionEvent::MessageUpdated { message } => {
                &message.session_id
            }
            SessionEvent::SessionUpdated { session } => &session.id,
            SessionEvent::RunUpdated { run } => &run.session_id,
            SessionEvent::BusinessToolRequest { request } => &request.session_id,
            SessionEvent::RunCleared { session_id } => session_id,
            SessionEvent::SessionDeleted { session_id } => session_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BusinessToolRequestStatus {
    Pending,
    Completed,
    Failed,
    Expired,
}

#[derive(Clone, Debug, Serialize)]
pub struct BusinessToolRequestEvent {
    pub request_id: String,
    pub session_id: String,
    pub integration_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub timeout_seconds: u64,
    pub expires_at: u64,
    pub status: BusinessToolRequestStatus,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_message_id: Option<String>,
}
