use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures_util::StreamExt;
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::{ChannelMessageRole, ChannelStore},
    docker_config_from_app,
    hermes::{
        docker_provisioner::{
            DockerProvisioner, DockerRuntime, DockerRuntimeOutput, NoopDockerRuntime,
        },
        event_streams::HermesEventStreamRegistry,
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
        provisioner::ProvisionerError,
        proxy_client::{
            DynHermesProxyClient, HermesProxyResponse, InMemoryHermesProxyClient,
            ReqwestHermesProxyClient,
        },
    },
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceExt;

fn test_state(store: SessionStore, proxy: InMemoryHermesProxyClient) -> AppState {
    test_state_with_proxy_and_channel_store(store, ChannelStore::default(), proxy.shared())
}

fn test_state_with_proxy(store: SessionStore, proxy: DynHermesProxyClient) -> AppState {
    test_state_with_proxy_and_channel_store(store, ChannelStore::default(), proxy)
}

fn test_state_with_proxy_and_channel_store(
    store: SessionStore,
    channel_store: ChannelStore,
    proxy: DynHermesProxyClient,
) -> AppState {
    let config = AppConfig::for_tests();
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store,
        channel_store,
        hermes_proxy: proxy,
        hermes_event_streams: HermesEventStreamRegistry::default(),
        model_registry: ready_model_registry(),
        llm_provider: InMemoryLlmProviderClient::new(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        })
        .shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    }
}

#[derive(Clone)]
struct FailingDockerRuntime;

#[async_trait::async_trait]
impl DockerRuntime for FailingDockerRuntime {
    async fn run(&self, _args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        Err(ProvisionerError::DockerRuntime(
            "docker command is unavailable".to_string(),
        ))
    }
}

fn ready_model_registry() -> ModelRegistry {
    ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://ready-provider.example/v1".to_string(),
        provider_api_key: "ready-provider-key".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        allow_streaming: true,
        request_timeout_seconds: 60,
    })
}

fn test_app(store: SessionStore, proxy: InMemoryHermesProxyClient) -> Router {
    build_router_with_state(test_state(store, proxy))
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("request can be built"),
        )
        .await
        .expect("router responds")
}

async fn request_empty(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    app.clone()
        .oneshot(builder.body(Body::empty()).expect("request can be built"))
        .await
        .expect("router responds")
}

async fn request_raw(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }

    app.clone()
        .oneshot(
            builder
                .body(Body::from(body))
                .expect("request can be built"),
        )
        .await
        .expect("router responds")
}

async fn response_json(response: Response<Body>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body can be read");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is json")
    };

    (status, value)
}

fn cookie_from(response: &Response<Body>) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response sets a cookie")
        .to_str()
        .expect("cookie is valid ascii")
        .split(';')
        .next()
        .expect("cookie has name and value")
        .to_string()
}

async fn bootstrap_and_login(app: &Router) -> String {
    let response = request_json(
        app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    cookie_from(&response)
}

fn managed_instance_for(user_id: &str) -> HermesInstance {
    HermesInstance {
        id: "instance-1".to_string(),
        user_id: user_id.to_string(),
        kind: HermesInstanceKind::ManagedDocker,
        status: HermesInstanceStatus::Running,
        name: "hermes-user-admin".to_string(),
        base_url: "http://hermes-user-admin:8000".to_string(),
        api_token_secret_ref: Some("hermes-secret-token".to_string()),
        llm_api_key: None,
        container_id: Some("container-1".to_string()),
        host_workspace_path: Some("/tmp/hermes/admin/workspace".to_string()),
        host_sandbox_path: Some("/tmp/hermes/admin/sandbox".to_string()),
        host_config_path: Some("/tmp/hermes/admin/config".to_string()),
        health_status: "healthy".to_string(),
    }
}

fn external_instance_with_base_url(user_id: &str, base_url: String) -> HermesInstance {
    HermesInstance {
        kind: HermesInstanceKind::External,
        container_id: None,
        host_workspace_path: None,
        host_sandbox_path: None,
        host_config_path: None,
        base_url,
        ..managed_instance_for(user_id)
    }
}

#[derive(Clone, Default)]
struct CapturedHermesRequest {
    authorization: Arc<Mutex<Option<String>>>,
    content_type: Arc<Mutex<Option<String>>>,
    uri: Arc<Mutex<Option<String>>>,
    body: Arc<Mutex<Option<Value>>>,
}

async fn hermes_handler(
    State(captured): State<CapturedHermesRequest>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    body: Body,
) -> impl IntoResponse {
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("hermes body can be read");
    *captured.authorization.lock().expect("auth lock") = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    *captured.content_type.lock().expect("content type lock") = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    *captured.uri.lock().expect("uri lock") = Some(uri.to_string());
    *captured.body.lock().expect("body lock") = serde_json::from_slice::<Value>(&bytes).ok();

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: message\ndata: real-hermes\n\n",
    )
}

async fn spawn_hermes_server(captured: CapturedHermesRequest) -> String {
    let app = Router::new()
        .route("/v1/runs", post(hermes_handler))
        .with_state(captured);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test hermes can bind");
    let addr = listener.local_addr().expect("test hermes addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test hermes server runs");
    });

    format!("http://{addr}")
}

async fn spawn_broken_event_stream_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test hermes can bind");
    let addr = listener.local_addr().expect("test hermes addr");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("test connection accepted");
        let mut request_buffer = [0_u8; 1024];
        let _ = socket.read(&mut request_buffer).await;
        let event = b"data: {\"event\":\"message.delta\",\"delta\":\"partial\"}\n\n";
        let headers = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n",
            event.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("headers can be written");
        socket.write_all(event).await.expect("event can be written");
        socket
            .write_all(b"\r\n")
            .await
            .expect("chunk trailer can be written");
        // 不写最后的 0 长度 chunk，模拟 Hermes 上游偶发 incomplete chunked read。
    });

    format!("http://{addr}")
}

#[derive(Default)]
struct SlowEventStreamState {
    accepted_connections: AtomicUsize,
    completed_connections: AtomicUsize,
    second_write_succeeded: AtomicBool,
    finished: Notify,
}

#[derive(Clone, Default)]
struct ApprovalEventStreamState {
    authorization: Arc<Mutex<Option<String>>>,
    approval_body: Arc<Mutex<Option<Value>>>,
    approval_seen: Arc<Notify>,
}

impl ApprovalEventStreamState {
    async fn wait_for_approval(&self) {
        while self
            .approval_body
            .lock()
            .expect("approval body lock")
            .is_none()
        {
            self.approval_seen.notified().await;
        }
    }
}

impl SlowEventStreamState {
    async fn wait_for_completion(&self) {
        while self.completed_connections.load(Ordering::SeqCst) == 0 {
            self.finished.notified().await;
        }
    }
}

async fn spawn_slow_event_stream_server() -> (String, Arc<SlowEventStreamState>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test hermes can bind");
    let addr = listener.local_addr().expect("test hermes addr");
    let state = Arc::new(SlowEventStreamState::default());
    let task_state = state.clone();

    tokio::spawn(async move {
        for _ in 0..2 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            task_state
                .accepted_connections
                .fetch_add(1, Ordering::SeqCst);
            let state = task_state.clone();

            tokio::spawn(async move {
                let mut request_buffer = [0_u8; 1024];
                let _ = socket.read(&mut request_buffer).await;
                let first = b"data: {\"event\":\"message.delta\",\"delta\":\"first\"}\n\n";
                let second = b"data: {\"event\":\"run.completed\",\"output\":\"first-second\"}\n\n";
                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n",
                    first.len()
                );

                if socket.write_all(headers.as_bytes()).await.is_err()
                    || socket.write_all(first).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    state.completed_connections.fetch_add(1, Ordering::SeqCst);
                    state.finished.notify_waiters();
                    return;
                }

                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let second_succeeded = socket
                    .write_all(format!("{:x}\r\n", second.len()).as_bytes())
                    .await
                    .is_ok()
                    && socket.write_all(second).await.is_ok()
                    && socket.write_all(b"\r\n0\r\n\r\n").await.is_ok();
                state
                    .second_write_succeeded
                    .store(second_succeeded, Ordering::SeqCst);
                state.completed_connections.fetch_add(1, Ordering::SeqCst);
                state.finished.notify_waiters();
            });
        }
    });

    (format!("http://{addr}"), state)
}

async fn approval_events_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "data: {\"event\":\"approval.request\",\"run_id\":\"run-approval\",\"command\":\"node -e \\\"dangerous\\\"\",\"choices\":[\"once\",\"session\",\"always\",\"deny\"]}\n\n",
    )
}

async fn approval_named_events_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: approval.request\ndata: {\"command\":\"node -e \\\"dangerous\\\"\",\"choices\":[\"once\",\"session\",\"always\",\"deny\"]}\n\n",
    )
}

async fn approval_split_events_handler() -> impl IntoResponse {
    let chunks = vec![
        Ok::<Bytes, Infallible>(Bytes::from_static(
            b"event: approval.request\ndata: {\"command\":\"node -e ",
        )),
        Ok::<Bytes, Infallible>(Bytes::from_static(
            b"\\\"dangerous\\\"\",\"choices\":[\"once\",\"session\",\"always\",\"deny\"]}\n\n",
        )),
    ];

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(futures_util::stream::iter(chunks)),
    )
}

async fn approval_run_create_handler() -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"run_id\":\"run-background\",\"status\":\"running\"}",
    )
}

async fn approval_response_handler(
    State(state): State<ApprovalEventStreamState>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("approval body can be read");
    *state.authorization.lock().expect("auth lock") = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    *state.approval_body.lock().expect("approval body lock") =
        serde_json::from_slice::<Value>(&bytes).ok();
    state.approval_seen.notify_waiters();

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"object\":\"hermes.run.approval_response\",\"run_id\":\"run-approval\",\"choice\":\"session\",\"resolved\":1}",
    )
}

async fn spawn_approval_event_stream_server(state: ApprovalEventStreamState) -> String {
    let app = Router::new()
        .route("/v1/runs", post(approval_run_create_handler))
        .route("/v1/runs/run-approval/events", get(approval_events_handler))
        .route(
            "/v1/runs/run-standard/events",
            get(approval_named_events_handler),
        )
        .route(
            "/v1/runs/run-split/events",
            get(approval_split_events_handler),
        )
        .route(
            "/v1/runs/run-background/events",
            get(approval_named_events_handler),
        )
        .route(
            "/v1/runs/run-approval/approval",
            post(approval_response_handler),
        )
        .route(
            "/v1/runs/run-standard/approval",
            post(approval_response_handler),
        )
        .route(
            "/v1/runs/run-split/approval",
            post(approval_response_handler),
        )
        .route(
            "/v1/runs/run-background/approval",
            post(approval_response_handler),
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test hermes can bind");
    let addr = listener.local_addr().expect("test hermes addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test hermes server runs");
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn hermes_proxy_test() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::OK,
        content_type: Some("text/event-stream".to_string()),
        body: "event: message\ndata: hello\n\n".as_bytes().to_vec(),
    });
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let create_channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (status, channel_body) = response_json(create_channel).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(channel_body["channels"][0]["name"], "hermes-hub");
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");

    let create_session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (status, session_body) = response_json(create_session).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(session_body["session"]["kind"], "agent");

    let proxied = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs?stream=true",
        json!({ "prompt": "hello" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(proxied.status(), StatusCode::OK);
    assert_eq!(
        proxied
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("stream content type")
            .to_str()
            .expect("header is ascii"),
        "text/event-stream"
    );
    let bytes = to_bytes(proxied.into_body(), usize::MAX)
        .await
        .expect("stream body can be read");
    assert_eq!(bytes, "event: message\ndata: hello\n\n");

    let forwarded = proxy.last_request().expect("request forwarded");
    assert_eq!(forwarded.method, Method::POST);
    assert_eq!(forwarded.path_and_query, "/v1/runs?stream=true");
    assert_eq!(forwarded.body, br#"{"prompt":"hello"}"#);
    assert_eq!(forwarded.instance_base_url, "http://hermes-user-admin:8000");
    assert_eq!(
        forwarded.authorization,
        Some("Bearer hermes-secret-token".to_string())
    );

    let denied = request_empty(&app, Method::GET, "/api/hermes/admin/config", Some(&cookie)).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    let encoded_denied = request_empty(
        &app,
        Method::GET,
        "/api/hermes/%69nternal/config",
        Some(&cookie),
    )
    .await;
    assert_eq!(encoded_denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        store.proxy_audit_count().await.expect("audit count"),
        1,
        "only the successful proxied request is audited in this test"
    );
}

#[tokio::test]
async fn managed_proxy_requires_successful_docker_ensure() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::ACCEPTED,
        content_type: Some("application/json".to_string()),
        body: br#"{"run_id":"run-existing","status":"running"}"#.to_vec(),
    });
    let store = SessionStore::default();
    let config = AppConfig::for_tests();
    let state = AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(FailingDockerRuntime),
        ),
        config,
        store: store.clone(),
        channel_store: ChannelStore::default(),
        hermes_proxy: proxy.clone().shared(),
        hermes_event_streams: HermesEventStreamRegistry::default(),
        model_registry: ready_model_registry(),
        llm_provider: InMemoryLlmProviderClient::new(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        })
        .shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    };
    let app = build_router_with_state(state);
    let user = store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("user can be created");
    let session_token = store
        .create_session(&user.id)
        .await
        .expect("session can be created");
    store
        .bind_hermes_instance(managed_instance_for(&user.id))
        .await
        .expect("existing instance can be bound");

    let response = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs",
        json!({ "input": "hello", "stream": true }),
        Some(&format!("hermes_hub_session={session_token}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        proxy.last_request().is_none(),
        "托管 Hermes ensure 失败时不能绕过 adapter 版本检查继续透传"
    );
}

#[tokio::test]
async fn hermes_runs_proxy_keeps_direct_request_body_unchanged() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"run_id":"run-1","status":"started"}"#.to_vec(),
    });
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    for message in [
        json!({"role": "user", "content": "第一轮问题", "attachments": []}),
        json!({"role": "assistant", "content": "第一轮回答", "attachments": []}),
        json!({"role": "user", "content": "当前问题", "attachments": []}),
    ] {
        let response = request_json(
            &app,
            Method::POST,
            &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
            message,
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let proxied = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs",
        json!({ "input": "当前问题", "stream": true, "session_id": session_id }),
        Some(&cookie),
    )
    .await;
    assert_eq!(proxied.status(), StatusCode::OK);

    let forwarded = proxy.last_request().expect("request forwarded");
    let body = serde_json::from_slice::<Value>(&forwarded.body).expect("forwarded body is json");
    assert_eq!(body["input"], "当前问题");
    assert_eq!(body["session_id"], session_id);
    assert!(body.get("instructions").is_none());
    assert!(body.get("conversation_history").is_none());

    let session = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}"),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    assert!(session_body["session"]["hermes_run_id"].is_null());
}

#[tokio::test]
async fn hermes_runs_proxy_preserves_existing_instructions_without_channel_protocol() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"run_id":"run-1","status":"started"}"#.to_vec(),
    });
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let proxied = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs",
        json!({
            "input": "当前问题",
            "stream": true,
            "session_id": session_id,
            "instructions": "保持简洁"
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(proxied.status(), StatusCode::OK);

    let forwarded = proxy.last_request().expect("request forwarded");
    let body = serde_json::from_slice::<Value>(&forwarded.body).expect("forwarded body is json");
    let instructions = body["instructions"].as_str().expect("instructions");
    assert_eq!(instructions, "保持简洁");
    assert!(body.get("conversation_history").is_none());
}

#[tokio::test]
async fn hermes_runs_proxy_does_not_register_channel_active_run() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::ACCEPTED,
        content_type: Some("application/json".to_string()),
        body: br#"{"run_id":"run-active","status":"running"}"#.to_vec(),
    });
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let created = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs",
        json!({ "input": "hello", "stream": true, "session_id": session_id }),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::ACCEPTED);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert!(active_body["active_run"].is_null());

    let session = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}"),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    assert!(session_body["session"]["hermes_run_id"].is_null());
}

#[tokio::test]
async fn hermes_proxy_uses_real_http_client_and_records_audit() {
    let captured = CapturedHermesRequest::default();
    let hermes_base_url = spawn_hermes_server(captured.clone()).await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let proxied = request_json(
        &real_app,
        Method::POST,
        "/api/hermes/v1/runs?stream=true",
        json!({ "prompt": "hello" }),
        Some(&cookie),
    )
    .await;

    assert_eq!(proxied.status(), StatusCode::OK);
    let bytes = to_bytes(proxied.into_body(), usize::MAX)
        .await
        .expect("stream body can be read");
    assert_eq!(bytes, "event: message\ndata: real-hermes\n\n");
    assert_eq!(
        captured.authorization.lock().expect("auth lock").as_deref(),
        Some("Bearer hermes-secret-token")
    );
    assert_eq!(
        captured.uri.lock().expect("uri lock").as_deref(),
        Some("/v1/runs?stream=true")
    );
    assert_eq!(
        captured
            .body
            .lock()
            .expect("body lock")
            .as_ref()
            .expect("body")["prompt"],
        "hello"
    );
    assert_eq!(store.proxy_audit_count().await.expect("audit count"), 1);
}

#[tokio::test]
async fn hermes_proxy_finishes_event_stream_when_upstream_chunk_errors() {
    let hermes_base_url = spawn_broken_event_stream_server().await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let proxied = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-broken/events",
        Some(&cookie),
    )
    .await;

    assert_eq!(proxied.status(), StatusCode::OK);
    let bytes = to_bytes(proxied.into_body(), usize::MAX)
        .await
        .expect("event stream body should complete despite upstream chunk error");
    assert_eq!(
        bytes,
        Bytes::from_static(b"data: {\"event\":\"message.delta\",\"delta\":\"partial\"}\n\n")
    );
}

#[tokio::test]
async fn hermes_proxy_keeps_upstream_event_stream_running_after_browser_disconnect() {
    let (hermes_base_url, slow_stream) = spawn_slow_event_stream_server().await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let first_response = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-slow/events",
        Some(&cookie),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let mut first_body = first_response.into_body().into_data_stream();
    let first_chunk = first_body
        .next()
        .await
        .expect("first event chunk is available")
        .expect("first event chunk is readable");
    assert_eq!(
        first_chunk,
        Bytes::from_static(b"data: {\"event\":\"message.delta\",\"delta\":\"first\"}\n\n")
    );
    drop(first_body);

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        slow_stream.wait_for_completion(),
    )
    .await
    .expect("upstream keeps running after browser body is dropped");
    assert!(slow_stream.second_write_succeeded.load(Ordering::SeqCst));

    let reconnected = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-slow/events",
        Some(&cookie),
    )
    .await;
    assert_eq!(reconnected.status(), StatusCode::OK);
    let bytes = to_bytes(reconnected.into_body(), usize::MAX)
        .await
        .expect("cached event stream can be replayed");
    assert_eq!(
        bytes,
        Bytes::from_static(
            b"data: {\"event\":\"message.delta\",\"delta\":\"first\"}\n\ndata: {\"event\":\"run.completed\",\"output\":\"first-second\"}\n\n"
        )
    );
    assert_eq!(
        slow_stream.accepted_connections.load(Ordering::SeqCst),
        1,
        "frontend reconnects should reuse Hub cache instead of opening a second Hermes stream",
    );
}

#[tokio::test]
async fn hermes_proxy_auto_approves_run_approval_requests() {
    let approval_state = ApprovalEventStreamState::default();
    let hermes_base_url = spawn_approval_event_stream_server(approval_state.clone()).await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let response = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-approval/events",
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("event stream body can be read");
    assert!(String::from_utf8_lossy(&body).contains("approval.request"));

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        approval_state.wait_for_approval(),
    )
    .await
    .expect("Hub auto-approves approval.request events");
    assert_eq!(
        approval_state
            .authorization
            .lock()
            .expect("auth lock")
            .as_deref(),
        Some("Bearer hermes-secret-token")
    );
    assert_eq!(
        approval_state
            .approval_body
            .lock()
            .expect("approval body lock")
            .clone(),
        Some(json!({ "choice": "session", "all": true }))
    );
}

#[tokio::test]
async fn hermes_proxy_auto_approves_standard_sse_approval_events() {
    let approval_state = ApprovalEventStreamState::default();
    let hermes_base_url = spawn_approval_event_stream_server(approval_state.clone()).await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let response = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-standard/events",
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("event stream body can be read");
    assert!(String::from_utf8_lossy(&body).contains("event: approval.request"));

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        approval_state.wait_for_approval(),
    )
    .await
    .expect("Hub auto-approves standard event/data SSE approval events");
    assert_eq!(
        approval_state
            .approval_body
            .lock()
            .expect("approval body lock")
            .clone(),
        Some(json!({ "choice": "session", "all": true }))
    );
}

#[tokio::test]
async fn hermes_proxy_auto_approves_split_chunk_approval_events() {
    let approval_state = ApprovalEventStreamState::default();
    let hermes_base_url = spawn_approval_event_stream_server(approval_state.clone()).await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");

    let response = request_empty(
        &real_app,
        Method::GET,
        "/api/hermes/v1/runs/run-split/events",
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("event stream body can be read");
    assert!(String::from_utf8_lossy(&body).contains("event: approval.request"));

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        approval_state.wait_for_approval(),
    )
    .await
    .expect("Hub auto-approves approval events split across upstream chunks");
    assert_eq!(
        approval_state
            .approval_body
            .lock()
            .expect("approval body lock")
            .clone(),
        Some(json!({ "choice": "session", "all": true }))
    );
}

#[tokio::test]
async fn hermes_proxy_does_not_start_background_events_after_direct_run_creation() {
    let approval_state = ApprovalEventStreamState::default();
    let hermes_base_url = spawn_approval_event_stream_server(approval_state.clone()).await;
    let store = SessionStore::default();
    let app = test_app(
        store.clone(),
        InMemoryHermesProxyClient::default(), // 临时构建后会用真实 proxy state 覆盖。
    );
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let real_state =
        test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let real_app = build_router_with_state(real_state);
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, hermes_base_url))
        .await
        .expect("instance can be bound");
    let channel = request_empty(&real_app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &real_app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let response = request_json(
        &real_app,
        Method::POST,
        "/api/hermes/v1/runs",
        json!({ "input": "需要自动批准的任务", "stream": true, "session_id": session_id }),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            approval_state.wait_for_approval(),
        )
        .await
        .is_err(),
        "直连 /api/hermes/v1/runs 不再后台接管 events；Hub 对话任务由 adapter 队列驱动"
    );
    assert!(approval_state
        .approval_body
        .lock()
        .expect("approval body lock")
        .is_none());
}

#[tokio::test]
async fn admin_can_update_external_hermes_config_used_by_proxy() {
    let first = CapturedHermesRequest::default();
    let first_base_url = spawn_hermes_server(first).await;
    let second = CapturedHermesRequest::default();
    let second_base_url = spawn_hermes_server(second.clone()).await;
    let store = SessionStore::default();
    let state = test_state_with_proxy(store.clone(), ReqwestHermesProxyClient::default().shared());
    let app = build_router_with_state(state);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(external_instance_with_base_url(&user_id, first_base_url))
        .await
        .expect("external instance can be bound");

    let updated = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/users/{user_id}/hermes-instance/external-config"),
        json!({
            "name": "admin external",
            "base_url": second_base_url,
            "api_token": "rotated-token"
        }),
        Some(&cookie),
    )
    .await;
    let (status, body) = response_json(updated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["name"], "admin external");

    let proxied = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs?stream=true",
        json!({ "prompt": "hello" }),
        Some(&cookie),
    )
    .await;

    assert_eq!(proxied.status(), StatusCode::OK);
    assert_eq!(
        second.authorization.lock().expect("auth lock").as_deref(),
        Some("Bearer rotated-token")
    );
}

#[tokio::test]
async fn channel_messages_and_attachments_are_hub_owned() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");

    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-hub-test-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"note.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         hello attachment\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&cookie),
        None,
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let attachment_id = upload_body["attachments"][0]["id"]
        .as_str()
        .expect("attachment id");
    assert_eq!(upload_body["attachments"][0]["name"], "note.txt");
    assert_eq!(upload_body["attachments"][0]["size"], 16);

    let appended = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "see attachment",
            "attachments": upload_body["attachments"].clone()
        }),
        Some(&cookie),
    )
    .await;
    let (status, appended_body) = response_json(appended).await;
    assert_eq!(status, StatusCode::CREATED);
    let message_id = appended_body["message"]["id"].as_str().expect("message id");
    assert_eq!(
        appended_body["message"]["attachments"][0]["message_id"],
        message_id
    );

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (status, messages_body) = response_json(messages).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["id"],
        attachment_id
    );
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["message_id"],
        messages_body["messages"][0]["id"]
    );

    let download = request_empty(
        &app,
        Method::GET,
        &format!("/api/attachments/{attachment_id}/download"),
        Some(&cookie),
    )
    .await;
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(
        download
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain")
    );
    assert!(download
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .expect("content disposition")
        .contains("filename=\"note.txt\""));
    let bytes = to_bytes(download.into_body(), usize::MAX)
        .await
        .expect("download body can be read");
    assert_eq!(&bytes[..], b"hello attachment");

    let ppt_name = "小学10以内加减法_6页配图.pptx";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"{ppt_name}\"\r\n\
         Content-Type: application/vnd.openxmlformats-officedocument.presentationml.presentation\r\n\r\n\
         pptx bytes\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&cookie),
        None,
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let ppt_attachment_id = upload_body["attachments"][0]["id"]
        .as_str()
        .expect("ppt attachment id");
    let download = request_empty(
        &app,
        Method::GET,
        &format!("/api/attachments/{ppt_attachment_id}/download"),
        Some(&cookie),
    )
    .await;
    let disposition = download
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .expect("utf8 content disposition");
    assert!(!disposition.contains("filename=\""));
    assert!(disposition.contains(
        "filename*=UTF-8''%E5%B0%8F%E5%AD%A610%E4%BB%A5%E5%86%85%E5%8A%A0%E5%87%8F%E6%B3%95_6%E9%A1%B5%E9%85%8D%E5%9B%BE.pptx"
    ));

    let encoded_ppt_name =
        "%E5%B0%8F%E5%AD%A610%E4%BB%A5%E5%86%85%E5%8A%A0%E5%87%8F%E6%B3%95_6%E9%A1%B5%E9%85%8D%E5%9B%BE.pptx";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"{encoded_ppt_name}\"\r\n\
         Content-Type: application/vnd.openxmlformats-officedocument.presentationml.presentation\r\n\r\n\
         encoded pptx bytes\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&cookie),
        None,
    )
    .await;
    let (status, encoded_upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        encoded_upload_body["attachments"][0]["name"],
        "小学10以内加减法_6页配图.pptx"
    );
}

#[tokio::test]
async fn channel_message_attachments_must_reference_uploaded_hub_objects() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let appended = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "伪造附件",
            "attachments": [{
                "id": "11111111-1111-1111-1111-111111111111",
                "name": "fake.txt",
                "download_url": "/api/attachments/11111111-1111-1111-1111-111111111111/download"
            }]
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(appended.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_channel_session_stops_active_run_and_removes_messages_and_files() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-hub-delete-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"delete-me.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         delete me\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&cookie),
        None,
    )
    .await;
    let (_, upload_body) = response_json(upload).await;
    let attachment = upload_body["attachments"][0].clone();
    let attachment_id = attachment["id"].as_str().expect("attachment id");
    let download_url = attachment["download_url"].as_str().expect("download url");

    let started = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "delete this conversation",
            "attachments": [attachment],
            "client_message_key": "delete-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (status, started_body) = response_json(started).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(started_body["run"]["run_id"]
        .as_str()
        .expect("run id")
        .starts_with("hub-run-"));

    let deleted = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/channels/{channel_id}/sessions/{session_id}"),
        Some(&cookie),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(
        proxy.last_request().is_none(),
        "删除 Hub 会话只取消 channel_run，不再调用原生 Hermes run stop"
    );

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    assert_eq!(messages.status(), StatusCode::NOT_FOUND);
    let download = request_empty(&app, Method::GET, download_url, Some(&cookie)).await;
    assert_eq!(download.status(), StatusCode::NOT_FOUND);
    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    assert_eq!(active.status(), StatusCode::NOT_FOUND);
    assert!(!attachment_id.is_empty());
}

#[tokio::test]
async fn assistant_message_with_container_image_path_keeps_text_without_output_attachment() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let image_name = "openai_gpt-image-2-medium_20260520_093515_f459c665.png";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let delivered = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": format!("生成好了：\n\n![赛博朋克猫](/config/cache/images/{image_name})"),
            "attachments": []
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    let saved_content = delivered_body["message"]["content"]
        .as_str()
        .expect("message content");
    assert_eq!(
        delivered_body["message"]["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );
    assert!(saved_content.contains("/config/cache/images/"));
    assert!(saved_content.contains("![赛博朋克猫]"));
}

#[tokio::test]
async fn listing_legacy_assistant_image_path_does_not_read_container_file() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let image_name = "openai_gpt-image-2-medium_20260520_093515_f459c665.png";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    // 模拟旧版本已经落库的历史消息：内容里仍是 Hermes 容器路径，没有 Hub 附件。
    state
        .channel_store
        .append_session_message(
            &user_id,
            channel_id,
            session_id,
            ChannelMessageRole::Assistant,
            None,
            format!("历史生成图：\n\n![旧图](/config/cache/images/{image_name})"),
            json!([]),
        )
        .await
        .expect("legacy message can be inserted");

    let listed = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let (_, listed_body) = response_json(listed).await;
    let message = &listed_body["messages"][0];
    assert!(message["content"]
        .as_str()
        .expect("message content")
        .contains("/config/cache/images/"));
    assert_eq!(
        message["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );

    let listed_again = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, listed_again_body) = response_json(listed_again).await;
    assert_eq!(
        listed_again_body["messages"][0]["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );
}

#[tokio::test]
async fn assistant_message_with_container_file_path_keeps_text_without_output_attachment() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let ppt_name = "math-10以内加减法.pptx";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let delivered = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": format!("PPT 已生成：/opt/data/{ppt_name}"),
            "attachments": []
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    assert!(delivered_body["message"]["content"]
        .as_str()
        .expect("message content")
        .contains("/opt/data/"));
    assert_eq!(
        delivered_body["message"]["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );
}

#[tokio::test]
async fn assistant_message_with_client_key_is_idempotent_without_recopying_container_path() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let ppt_name = "math-idempotent.pptx";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let payload = json!({
        "role": "assistant",
        "client_message_key": "hermes-run:run-idempotent",
        "content": format!("PPT 已生成：/opt/data/{ppt_name}"),
        "attachments": []
    });
    let delivered = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        payload.clone(),
        Some(&cookie),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    let first_message_id = delivered_body["message"]["id"].clone();
    assert_eq!(
        delivered_body["message"]["client_message_key"],
        "hermes-run:run-idempotent"
    );
    assert_eq!(
        delivered_body["message"]["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );

    let delivered_again = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        payload,
        Some(&cookie),
    )
    .await;
    assert_eq!(delivered_again.status(), StatusCode::CREATED);
    let (_, delivered_again_body) = response_json(delivered_again).await;
    assert_eq!(delivered_again_body["message"]["id"], first_message_id);
    assert_eq!(
        delivered_again_body["message"]["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        0
    );

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, messages_body) = response_json(messages).await;
    assert_eq!(
        messages_body["messages"]
            .as_array()
            .expect("messages")
            .len(),
        1
    );
}

#[tokio::test]
async fn hermes_instance_can_deliver_channel_message_to_hub() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-channel-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let delivered = request_json(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": "artifact ready",
            "attachments": []
        }),
        None,
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::UNAUTHORIZED);

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        br#"{"role":"user","content":"forged user input","attachments":[]}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::BAD_REQUEST);

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        br#"{"role":"assistant","content":"artifact ready","attachments":[]}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, messages_body) = response_json(messages).await;
    assert_eq!(messages_body["messages"][0]["role"], "assistant");
    assert_eq!(messages_body["messages"][0]["content"], "artifact ready");
}

#[tokio::test]
async fn channel_session_events_stream_snapshot_and_adapter_messages() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-session-events-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let stored = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": "stored answer",
            "attachments": []
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(stored.status(), StatusCode::CREATED);

    let stream_response = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/events"),
        Some(&cookie),
    )
    .await;
    assert_eq!(stream_response.status(), StatusCode::OK);
    assert_eq!(
        stream_response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let mut body = stream_response.into_body().into_data_stream();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
        .await
        .expect("snapshot event arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot chunk is readable");
    let snapshot_text = String::from_utf8_lossy(&snapshot);
    assert!(snapshot_text.contains("messages_snapshot"));
    assert!(snapshot_text.contains("stored answer"));

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "live adapter answer",
            "attachments": [],
            "client_message_key": "adapter-live-message"
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);

    let live = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
        .await
        .expect("live event arrives")
        .expect("live chunk exists")
        .expect("live chunk is readable");
    let live_text = String::from_utf8_lossy(&live);
    assert!(live_text.contains("message_created"));
    assert!(live_text.contains("live adapter answer"));
}

#[tokio::test]
async fn hermes_channel_protocol_uploads_output_file_before_delivering_message() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-channel-file-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-channel-file-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"结果.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         123\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let attachment = upload_body["attachments"][0].clone();
    assert_eq!(attachment["direction"], "output");
    assert_eq!(attachment["name"], "结果.txt");
    assert_eq!(attachment["content_type"], "text/plain");
    let attachment_id = attachment["id"]
        .as_str()
        .expect("attachment id")
        .to_string();
    let download_url = attachment["download_url"]
        .as_str()
        .expect("download url")
        .to_string();

    let delivered = request_json(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": format!("文件已生成：[结果.txt]({download_url})"),
            "attachments": [attachment]
        }),
        None,
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::UNAUTHORIZED);

    let forged = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "伪造附件不会通过",
            "attachments": [{
                "id": "22222222-2222-2222-2222-222222222222",
                "name": "fake.txt",
                "download_url": "/api/attachments/22222222-2222-2222-2222-222222222222/download"
            }]
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(forged.status(), StatusCode::NOT_FOUND);

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": format!("文件已生成：[结果.txt]({download_url})"),
            "attachments": [{
                "id": attachment_id,
                "name": "伪造名称.txt",
                "content_type": "application/x-forged",
                "download_url": "/api/attachments/forged/download"
            }]
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    let message_id = delivered_body["message"]["id"]
        .as_str()
        .expect("message id");
    assert_eq!(
        delivered_body["message"]["attachments"][0]["message_id"],
        message_id
    );
    assert_eq!(
        delivered_body["message"]["attachments"][0]["name"],
        "结果.txt"
    );
    assert_eq!(
        delivered_body["message"]["attachments"][0]["content_type"],
        "text/plain"
    );
    assert_eq!(
        delivered_body["message"]["attachments"][0]["download_url"],
        download_url
    );

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (status, messages_body) = response_json(messages).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        messages_body["messages"][0]["content"],
        format!("文件已生成：[结果.txt]({download_url})")
    );
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["name"],
        "结果.txt"
    );
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["message_id"],
        messages_body["messages"][0]["id"]
    );

    let download = request_empty(&app, Method::GET, &download_url, Some(&cookie)).await;
    assert_eq!(download.status(), StatusCode::OK);
    let bytes = to_bytes(download.into_body(), usize::MAX)
        .await
        .expect("download body");
    assert_eq!(&bytes[..], b"123");
}

#[tokio::test]
async fn hermes_channel_protocol_accepts_large_output_files_within_config_limit() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-channel-large-file-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state);
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-channel-large-file-boundary";
    let mut upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"large.pptx\"\r\n\
         Content-Type: application/vnd.openxmlformats-officedocument.presentationml.presentation\r\n\r\n"
    )
    .into_bytes();
    // 真实 PPT 回归约 12MB；这里用超过 Axum 默认 2MB、低于业务 25MB 上限的载荷覆盖路由体限制。
    let payload = vec![b'a'; 3 * 1024 * 1024];
    upload_body.extend_from_slice(&payload);
    upload_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(upload_body["attachments"][0]["size"], payload.len());
}

#[tokio::test]
async fn assistant_message_binds_hub_attachment_referenced_in_content() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-channel-linked-file-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-channel-linked-file-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"结果.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         123\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let attachment_id = upload_body["attachments"][0]["id"]
        .as_str()
        .expect("attachment id")
        .to_string();
    let download_url = upload_body["attachments"][0]["download_url"]
        .as_str()
        .expect("download url")
        .to_string();

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            // Hermes 的最终回答可能只包含 Hub 下载链接，不再额外回传 attachments 数组；
            // Hub 必须从内容里的标准下载 URL 自动恢复附件关系。
            "content": format!("文件已生成：[结果.txt]({download_url})"),
            "attachments": []
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    let message_id = delivered_body["message"]["id"]
        .as_str()
        .expect("message id");
    assert_eq!(
        delivered_body["message"]["attachments"][0]["id"],
        attachment_id
    );
    assert_eq!(
        delivered_body["message"]["attachments"][0]["message_id"],
        message_id
    );

    let attachment = state
        .channel_store
        .get_attachment(&user_id, &attachment_id)
        .await
        .expect("attachment can be read");
    assert_eq!(attachment.message_id.as_deref(), Some(message_id));

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (status, messages_body) = response_json(messages).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["id"],
        attachment_id
    );
}

#[tokio::test]
async fn channel_inbox_waits_briefly_when_no_runs_are_ready() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-empty-inbox-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let started = std::time::Instant::now();

    let inbox = request_raw(
        &app,
        Method::GET,
        "/internal/channel/v1/inbox?timeout_seconds=1&limit=4",
        "application/json",
        Vec::new(),
        None,
        Some(instance_token),
    )
    .await;
    let elapsed = started.elapsed();
    let (status, inbox_body) = response_json(inbox).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox_body["items"].as_array().expect("items").len(), 0);
    assert!(
        elapsed >= std::time::Duration::from_millis(200),
        "empty Hub inbox polls must not spin in a tight loop"
    );
}

#[tokio::test]
async fn channel_run_enqueue_can_be_polled_and_completed_by_hermes_hub_adapter() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-adapter-queue-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let boundary = "hermes-adapter-input-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"题目.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         1+1\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&cookie),
        None,
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    let input_attachment = upload_body["attachments"][0].clone();
    let input_attachment_id = input_attachment["id"]
        .as_str()
        .expect("input attachment id");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请读取附件并回答",
            "attachments": [input_attachment],
            "client_message_key": "user-turn-1"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created_run_body["message"]["role"], "user");
    assert_eq!(
        created_run_body["message"]["attachments"][0]["id"],
        input_attachment_id
    );
    assert_eq!(created_run_body["run"]["status"], "queued");
    let run_id = created_run_body["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    assert!(run_id.starts_with("hub-run-"));

    let created_again = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请读取附件并回答",
            "attachments": [input_attachment],
            "client_message_key": "user-turn-1"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_again_body) = response_json(created_again).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created_again_body["message"]["id"],
        created_run_body["message"]["id"]
    );
    assert_eq!(created_again_body["run"]["run_id"], run_id);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active_body["active_run"]["run_id"], run_id);
    assert_eq!(active_body["active_run"]["status"], "queued");

    let inbox = request_empty(
        &app,
        Method::GET,
        "/internal/channel/v1/inbox?timeout_seconds=0&limit=4",
        None,
    )
    .await;
    assert_eq!(inbox.status(), StatusCode::UNAUTHORIZED);

    let inbox = request_raw(
        &app,
        Method::GET,
        "/internal/channel/v1/inbox?timeout_seconds=0&limit=4",
        "application/json",
        Vec::new(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, inbox_body) = response_json(inbox).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox_body["items"].as_array().expect("items").len(), 1);
    let item = &inbox_body["items"][0];
    assert_eq!(item["id"], run_id);
    assert_eq!(item["session_id"], session_id);
    assert_eq!(item["content"], "请读取附件并回答");
    assert_eq!(item["attachments"][0]["id"], input_attachment_id);
    assert!(item["attachments"][0]["download_url"]
        .as_str()
        .expect("internal download url")
        .starts_with("/internal/channel/v1/attachments/"));

    let internal_download = request_raw(
        &app,
        Method::GET,
        item["attachments"][0]["download_url"]
            .as_str()
            .expect("download url"),
        "application/octet-stream",
        Vec::new(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(internal_download.status(), StatusCode::OK);
    let bytes = to_bytes(internal_download.into_body(), usize::MAX)
        .await
        .expect("download body");
    assert_eq!(&bytes[..], b"1+1");

    let status_update = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/runs/{run_id}/status"),
        "application/json",
        br#"{"status":"running"}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(status_update.status(), StatusCode::OK);

    let progress = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "🔧 terminal([\"command\"])\n{\"command\":\"python build.py\"}",
            "attachments": []
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(progress.status(), StatusCode::CREATED);
    let (_, progress_body) = response_json(progress).await;
    let progress_message_id = progress_body["message"]["id"]
        .as_str()
        .expect("progress message id")
        .to_string();

    let updated_progress = request_raw(
        &app,
        Method::PUT,
        &format!(
            "/internal/channel/v1/sessions/{session_id}/messages/{progress_message_id}"
        ),
        "application/json",
        json!({
            "content": "🔧 terminal([\"command\"])\n{\"command\":\"python build.py\"}\n✅ terminal completed",
            "attachments": []
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(updated_progress.status(), StatusCode::OK);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        active_body["active_run"]["status"], "running",
        "tool-progress messages must not complete the Hub run"
    );

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "附件里的算式结果是 2",
            "attachments": [],
            "client_message_key": format!("hermes-run:{run_id}")
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);
    let (_, delivered_body) = response_json(delivered).await;
    let assistant_message_id = delivered_body["message"]["id"]
        .as_str()
        .expect("assistant message id")
        .to_string();

    let delivered_again = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "附件里的算式结果是 2",
            "attachments": [],
            "client_message_key": format!("hermes-run:{run_id}")
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, delivered_again_body) = response_json(delivered_again).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(delivered_again_body["message"]["id"], assistant_message_id);

    let ack = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/inbox/{run_id}/ack"),
        "application/json",
        json!({}).to_string().into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(ack.status(), StatusCode::OK);

    let late_running_update = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/runs/{run_id}/status"),
        "application/json",
        br#"{"status":"running"}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    let (_, late_running_body) = response_json(late_running_update).await;
    assert_eq!(
        late_running_body["run"]["status"], "completed",
        "terminal Hub runs must not be moved back to running by late adapter status calls"
    );

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active_body["active_run"]["run_id"], run_id);
    assert_eq!(active_body["active_run"]["status"], "completed");
    assert_eq!(
        active_body["active_run"]["output_message_id"],
        assistant_message_id
    );

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, messages_body) = response_json(messages).await;
    assert_eq!(
        messages_body["messages"]
            .as_array()
            .expect("messages")
            .len(),
        3
    );
    assert_eq!(messages_body["messages"][1]["id"], progress_message_id);
    assert_eq!(
        messages_body["messages"][1]["content"],
        "🔧 terminal([\"command\"])\n{\"command\":\"python build.py\"}\n✅ terminal completed"
    );
    assert_eq!(messages_body["messages"][2]["id"], assistant_message_id);
}

#[tokio::test]
async fn assistant_message_with_hermes_run_key_does_not_clear_active_run() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-hermes-run-key-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请输出最终答案",
            "client_message_key": "hermes-run-key-user-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (_, created_run_body) = response_json(created_run).await;
    let run_id = created_run_body["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_string();

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "最终答案",
            "attachments": [],
            "client_message_key": format!("hermes-run:{run_id}")
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::CREATED);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active_body["active_run"]["run_id"], run_id);
    assert_eq!(active_body["active_run"]["status"], "queued");
}

#[tokio::test]
async fn terminal_adapter_run_remains_visible_until_browser_clears_it() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-adapter-terminal-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请执行一个会失败的任务",
            "client_message_key": "terminal-failed-user-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = created_run_body["run"]["run_id"].as_str().expect("run id");

    let failed = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/runs/{run_id}/status"),
        "application/json",
        br#"{"status":"failed","error":"tool failed"}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, failed_body) = response_json(failed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(failed_body["run"]["status"], "failed");

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active_body["active_run"]["run_id"], run_id);
    assert_eq!(active_body["active_run"]["status"], "failed");
    assert_eq!(active_body["active_run"]["error"], "tool failed");

    let cleared = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert!(active_body["active_run"].is_null());
}

#[tokio::test]
async fn late_adapter_output_after_stop_does_not_create_message() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-late-output-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请生成一个会被停止的文件",
            "client_message_key": "late-output-user-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = created_run_body["run"]["run_id"].as_str().expect("run id");

    let progress = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "执行中",
            "attachments": [],
            "run_id": run_id
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(progress.status(), StatusCode::CREATED);
    let (_, progress_body) = response_json(progress).await;
    let progress_message_id = progress_body["message"]["id"]
        .as_str()
        .expect("progress message id");

    let stopped = request_empty(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run/stop"),
        Some(&cookie),
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::OK);

    let late_edit = request_raw(
        &app,
        Method::PUT,
        &format!("/internal/channel/v1/sessions/{session_id}/messages/{progress_message_id}"),
        "application/json",
        json!({
            "content": "停止后的迟到编辑",
            "attachments": [],
            "run_id": run_id
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(late_edit.status(), StatusCode::CONFLICT);

    let late_output = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "迟到的最终输出",
            "attachments": [],
            "client_message_key": format!("hermes-run:{run_id}")
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(late_output.status(), StatusCode::CONFLICT);

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, messages_body) = response_json(messages).await;
    assert_eq!(
        messages_body["messages"]
            .as_array()
            .expect("messages")
            .len(),
        2,
        "停止后的迟到 Hermes 输出不能再写入会话"
    );
    assert_eq!(messages_body["messages"][0]["role"], "user");
    assert_eq!(messages_body["messages"][1]["content"], "执行中");
}

#[tokio::test]
async fn completed_adapter_run_exposes_output_message_id_until_cleared() {
    let proxy = InMemoryHermesProxyClient::default();
    let store = SessionStore::default();
    let state = test_state(store.clone(), proxy);
    let instance_token = "instance-adapter-completed-token";
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie)).await;
    let (_, channel_body) = response_json(channel).await;
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");
    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "请输出最终答案",
            "client_message_key": "terminal-completed-user-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = created_run_body["run"]["run_id"].as_str().expect("run id");

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "最终答案",
            "attachments": []
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, delivered_body) = response_json(delivered).await;
    assert_eq!(status, StatusCode::CREATED);
    let assistant_message_id = delivered_body["message"]["id"]
        .as_str()
        .expect("assistant message id");

    let ack = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/inbox/{run_id}/ack"),
        "application/json",
        json!({ "output_message_id": assistant_message_id })
            .to_string()
            .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(ack.status(), StatusCode::OK);

    let active = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    let (status, active_body) = response_json(active).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active_body["active_run"]["run_id"], run_id);
    assert_eq!(active_body["active_run"]["status"], "completed");
    assert_eq!(
        active_body["active_run"]["output_message_id"],
        assistant_message_id
    );

    let cleared = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run"),
        Some(&cookie),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
}
