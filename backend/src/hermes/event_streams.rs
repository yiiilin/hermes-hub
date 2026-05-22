use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, Mutex, Notify};

use crate::hermes::proxy_client::{DynHermesProxyClient, HermesProxyError, HermesProxyRequest};

const EVENT_REPLAY_TTL: Duration = Duration::from_secs(15 * 60);

/// Hub 托管 Hermes run event 流，避免浏览器 SSE 断开时连带取消后端到 Hermes 的连接。
#[derive(Clone, Default)]
pub struct HermesEventStreamRegistry {
    streams: Arc<Mutex<HashMap<String, Arc<ManagedHermesEventStream>>>>,
}

impl HermesEventStreamRegistry {
    pub async fn open(
        &self,
        key: String,
        received_bytes: usize,
        client: DynHermesProxyClient,
        request: HermesProxyRequest,
        run_ref: Option<HermesRunReference>,
    ) -> Result<Response, HermesProxyError> {
        let stream = self.stream_for(key, client, request, run_ref).await;
        let initial = stream.initial().await?;
        let body = stream.body_after(received_bytes).await;
        let mut response = Response::builder()
            .status(initial.status)
            .body(body)
            .map_err(|error| HermesProxyError::Failed(error.to_string()))?;

        copy_event_stream_headers(&initial.headers, initial.status, response.headers_mut());
        Ok(response)
    }

    async fn stream_for(
        &self,
        key: String,
        client: DynHermesProxyClient,
        request: HermesProxyRequest,
        run_ref: Option<HermesRunReference>,
    ) -> Arc<ManagedHermesEventStream> {
        let mut streams = self.streams.lock().await;
        if let Some(stream) = streams.get(&key) {
            return stream.clone();
        }

        let stream = Arc::new(ManagedHermesEventStream::new());
        streams.insert(key.clone(), stream.clone());

        let registry = self.clone();
        let stream_for_task = stream.clone();
        tokio::spawn(async move {
            run_upstream_event_stream(
                registry.clone(),
                stream_for_task.clone(),
                client,
                request,
                run_ref,
            )
            .await;

            // run 结束后保留短时间缓存，给前端切页/网络抖动后的重连留恢复窗口。
            tokio::time::sleep(EVENT_REPLAY_TTL).await;
            let mut streams = registry.streams.lock().await;
            if streams
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &stream_for_task))
            {
                streams.remove(&key);
            }
        });

        stream
    }

    pub async fn start_background(
        &self,
        key: String,
        client: DynHermesProxyClient,
        request: HermesProxyRequest,
        run_ref: Option<HermesRunReference>,
    ) {
        // 主动启动上游 SSE 连接，但不绑定任何浏览器响应体。
        // 这样前端刷新、后台切页或尚未打开 events 时，Hub 仍能持续接收 approval/run 事件。
        let _ = self.stream_for(key, client, request, run_ref).await;
    }

    async fn ingest_run_events(
        &self,
        auto_approval: &Option<HermesAutoApproval>,
        events: Vec<Value>,
    ) {
        for event in events {
            let event_name = event
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match event_name {
                "approval.request" => {
                    auto_approve_hermes_run(auto_approval.clone());
                }
                _ => {}
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct HermesRunReference {
    pub user_id: String,
    pub instance_id: String,
    pub run_id: String,
}

#[derive(Clone)]
struct HermesAutoApproval {
    client: DynHermesProxyClient,
    request: HermesProxyRequest,
}

#[derive(Clone, Debug, Serialize)]
pub struct HermesSessionRun {
    pub run_id: String,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub output_message_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

struct ManagedHermesEventStream {
    initial: Mutex<Option<Result<EventStreamInitial, String>>>,
    initial_ready: Notify,
    cache: Mutex<EventStreamCache>,
    parser: Mutex<EventStreamParser>,
    sender: broadcast::Sender<EventStreamSignal>,
}

#[derive(Clone)]
struct EventStreamInitial {
    status: StatusCode,
    headers: HeaderMap,
}

#[derive(Clone)]
enum EventStreamSignal {
    Chunk { seq: u64, bytes: Bytes },
    Finished,
}

#[derive(Default)]
struct EventStreamCache {
    chunks: Vec<(u64, Bytes)>,
    next_seq: u64,
    total_bytes: usize,
    finished: bool,
}

#[derive(Default)]
struct EventStreamParser {
    pending_bytes: Vec<u8>,
    event_name: String,
    data_lines: Vec<String>,
}

struct EventStreamSnapshot {
    chunks: Vec<(u64, Bytes)>,
    last_seq: u64,
    total_bytes: usize,
    finished: bool,
}

struct EventBodyState {
    stream: Arc<ManagedHermesEventStream>,
    receiver: broadcast::Receiver<EventStreamSignal>,
    pending: VecDeque<(u64, Bytes)>,
    last_seq: u64,
    remaining_skip_bytes: usize,
    finished: bool,
}

impl ManagedHermesEventStream {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            initial: Mutex::new(None),
            initial_ready: Notify::new(),
            cache: Mutex::new(EventStreamCache {
                next_seq: 1,
                ..EventStreamCache::default()
            }),
            parser: Mutex::new(EventStreamParser::default()),
            sender,
        }
    }

    async fn initial(&self) -> Result<EventStreamInitial, HermesProxyError> {
        loop {
            if let Some(initial) = self.initial.lock().await.clone() {
                return initial.map_err(HermesProxyError::Failed);
            }
            self.initial_ready.notified().await;
        }
    }

    async fn set_initial(&self, initial: Result<EventStreamInitial, String>) {
        let mut current = self.initial.lock().await;
        if current.is_none() {
            *current = Some(initial);
            self.initial_ready.notify_waiters();
        }
    }

    async fn push_chunk(&self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }

        let seq = {
            let mut cache = self.cache.lock().await;
            if cache.finished {
                return;
            }
            let seq = cache.next_seq;
            cache.next_seq += 1;
            cache.total_bytes += bytes.len();
            cache.chunks.push((seq, bytes.clone()));
            seq
        };

        let _ = self.sender.send(EventStreamSignal::Chunk { seq, bytes });
    }

    async fn parse_events(&self, bytes: &Bytes) -> Vec<Value> {
        self.parser.lock().await.push(bytes)
    }

    async fn flush_events(&self) -> Vec<Value> {
        self.parser.lock().await.flush()
    }

    async fn finish(&self) {
        {
            let mut cache = self.cache.lock().await;
            if cache.finished {
                return;
            }
            cache.finished = true;
        }
        let _ = self.sender.send(EventStreamSignal::Finished);
    }

    async fn snapshot(&self) -> EventStreamSnapshot {
        let cache = self.cache.lock().await;
        EventStreamSnapshot {
            chunks: cache.chunks.clone(),
            last_seq: cache.chunks.last().map(|(seq, _)| *seq).unwrap_or(0),
            total_bytes: cache.total_bytes,
            finished: cache.finished,
        }
    }

    async fn body_after(self: &Arc<Self>, received_bytes: usize) -> Body {
        let receiver = self.sender.subscribe();
        let snapshot = self.snapshot().await;
        let pending = chunks_after_received_bytes(&snapshot.chunks, received_bytes);
        let remaining_skip_bytes = received_bytes.saturating_sub(snapshot.total_bytes);
        let body_state = EventBodyState {
            stream: self.clone(),
            receiver,
            pending,
            last_seq: snapshot.last_seq,
            remaining_skip_bytes,
            finished: snapshot.finished,
        };

        Body::from_stream(stream::unfold(body_state, |mut state| async move {
            next_body_chunk(&mut state)
                .await
                .map(|chunk| (chunk, state))
        }))
    }
}

impl EventStreamParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.pending_bytes.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some(line_end) = self.pending_bytes.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending_bytes.drain(..=line_end).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.ingest_line(&line, &mut events);
        }

        events
    }

    fn flush(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if !self.pending_bytes.is_empty() {
            let line = std::mem::take(&mut self.pending_bytes);
            self.ingest_line(&line, &mut events);
        }
        if let Some(event) = self.take_pending_event() {
            events.push(event);
        }
        events
    }

    fn ingest_line(&mut self, line: &[u8], events: &mut Vec<Value>) {
        if line.is_empty() {
            if let Some(event) = self.take_pending_event() {
                events.push(event);
            }
            return;
        }

        let line = String::from_utf8_lossy(line);
        if line.starts_with(':') {
            return;
        }
        let Some((field, value)) = line.split_once(':') else {
            return;
        };
        let value = value.strip_prefix(' ').unwrap_or(value);

        match field {
            "event" => self.event_name = value.to_string(),
            "data" => self.data_lines.push(value.to_string()),
            _ => {}
        }
    }

    fn take_pending_event(&mut self) -> Option<Value> {
        let event_name = std::mem::take(&mut self.event_name);
        let data = std::mem::take(&mut self.data_lines).join("\n");
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return None;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(mut event) => {
                if event.get("event").is_none() && !event_name.is_empty() {
                    if let Some(object) = event.as_object_mut() {
                        object.insert("event".to_string(), Value::String(event_name));
                    }
                }
                Some(event)
            }
            Err(_) if !event_name.is_empty() => Some(serde_json::json!({
                "event": event_name,
                "message": data,
            })),
            Err(_) => None,
        }
    }
}

async fn run_upstream_event_stream(
    registry: HermesEventStreamRegistry,
    stream: Arc<ManagedHermesEventStream>,
    client: DynHermesProxyClient,
    request: HermesProxyRequest,
    run_ref: Option<HermesRunReference>,
) {
    let auto_approval = auto_approval_request(&client, &request, &run_ref);
    let response = match client.send(request).await {
        Ok(response) => response,
        Err(error) => {
            stream.set_initial(Err(error.to_string())).await;
            stream.finish().await;
            return;
        }
    };

    let (parts, body) = response.into_parts();
    stream
        .set_initial(Ok(EventStreamInitial {
            status: parts.status,
            headers: parts.headers,
        }))
        .await;

    let mut data_stream = body.into_data_stream();
    while let Some(item) = data_stream.next().await {
        match item {
            Ok(bytes) => {
                // 直连 Hermes SSE 只保留流缓存和自动批准能力；Hub 会话状态由 channel_runs 持久化。
                let events = stream.parse_events(&bytes).await;
                registry.ingest_run_events(&auto_approval, events).await;
                stream.push_chunk(bytes).await;
            }
            Err(error) => {
                // 上游读错误只结束 Hub 托管流，不再把 load failed 传播给浏览器。
                tracing::warn!(error = %error, "managed hermes event stream ended with read error");
                break;
            }
        }
    }

    let events = stream.flush_events().await;
    registry.ingest_run_events(&auto_approval, events).await;
    stream.finish().await;
}

fn auto_approval_request(
    client: &DynHermesProxyClient,
    event_stream_request: &HermesProxyRequest,
    run_ref: &Option<HermesRunReference>,
) -> Option<HermesAutoApproval> {
    let run_ref = run_ref.as_ref()?;
    Some(HermesAutoApproval {
        client: client.clone(),
        request: HermesProxyRequest {
            method: Method::POST,
            instance_base_url: event_stream_request.instance_base_url.clone(),
            path_and_query: format!("/v1/runs/{}/approval", run_ref.run_id),
            authorization: event_stream_request.authorization.clone(),
            content_type: Some("application/json".to_string()),
            body: br#"{"choice":"session","all":true}"#.to_vec(),
            timeout_seconds: event_stream_request.timeout_seconds,
        },
    })
}

fn auto_approve_hermes_run(auto_approval: Option<HermesAutoApproval>) {
    let Some(auto_approval) = auto_approval else {
        return;
    };

    tokio::spawn(async move {
        // 外部 Hermes 也通过 Hub 走统一策略：遇到 approval.request 自动批准当前会话所有待处理项。
        match auto_approval.client.send(auto_approval.request).await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(
                    status = %response.status(),
                    "hermes auto approval request was not accepted"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "hermes auto approval request failed");
            }
        }
    });
}

async fn next_body_chunk(state: &mut EventBodyState) -> Option<Result<Bytes, io::Error>> {
    if let Some(bytes) = next_pending_chunk(state) {
        return Some(Ok(bytes));
    }

    if state.finished {
        return None;
    }

    loop {
        match state.receiver.recv().await {
            Ok(EventStreamSignal::Chunk { seq, bytes }) if seq > state.last_seq => {
                state.last_seq = seq;
                if state.remaining_skip_bytes >= bytes.len() {
                    state.remaining_skip_bytes -= bytes.len();
                    continue;
                }

                if state.remaining_skip_bytes > 0 {
                    let offset = state.remaining_skip_bytes;
                    state.remaining_skip_bytes = 0;
                    return Some(Ok(bytes.slice(offset..)));
                }

                return Some(Ok(bytes));
            }
            Ok(EventStreamSignal::Chunk { .. }) => {}
            Ok(EventStreamSignal::Finished) => {
                state.finished = true;
                return None;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                refill_lagged_pending(state).await;
                if let Some(bytes) = next_pending_chunk(state) {
                    return Some(Ok(bytes));
                }
                if state.finished {
                    return None;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

fn next_pending_chunk(state: &mut EventBodyState) -> Option<Bytes> {
    while let Some((seq, bytes)) = state.pending.pop_front() {
        state.last_seq = state.last_seq.max(seq);
        if state.remaining_skip_bytes >= bytes.len() {
            state.remaining_skip_bytes -= bytes.len();
            continue;
        }

        if state.remaining_skip_bytes > 0 {
            let offset = state.remaining_skip_bytes;
            state.remaining_skip_bytes = 0;
            return Some(bytes.slice(offset..));
        }

        return Some(bytes);
    }

    None
}

async fn refill_lagged_pending(state: &mut EventBodyState) {
    let snapshot = state.stream.snapshot().await;
    state.pending = snapshot
        .chunks
        .into_iter()
        .filter(|(seq, _)| *seq > state.last_seq)
        .collect();
    state.finished = snapshot.finished;
}

fn chunks_after_received_bytes(
    chunks: &[(u64, Bytes)],
    received_bytes: usize,
) -> VecDeque<(u64, Bytes)> {
    let mut remaining = received_bytes;
    let mut pending = VecDeque::new();

    for (seq, bytes) in chunks {
        if remaining >= bytes.len() {
            remaining -= bytes.len();
            continue;
        }

        if remaining > 0 {
            pending.push_back((*seq, bytes.slice(remaining..)));
            remaining = 0;
        } else {
            pending.push_back((*seq, bytes.clone()));
        }
    }

    pending
}

fn copy_event_stream_headers(upstream: &HeaderMap, status: StatusCode, downstream: &mut HeaderMap) {
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::RETRY_AFTER,
        HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = upstream.get(&name) {
            downstream.insert(name, value.clone());
        }
    }

    if status.is_success() && !downstream.contains_key(header::CONTENT_TYPE) {
        downstream.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
    }
}
