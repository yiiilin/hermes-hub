use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::{
        docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
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
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn test_state(store: SessionStore, proxy: InMemoryHermesProxyClient) -> AppState {
    test_state_with_proxy(store, proxy.shared())
}

fn test_state_with_proxy(store: SessionStore, proxy: DynHermesProxyClient) -> AppState {
    let config = AppConfig::for_tests();
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store,
        channel_store: ChannelStore::default(),
        hermes_proxy: proxy,
        model_registry: ready_model_registry(),
        llm_provider: InMemoryLlmProviderClient::new(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        })
        .shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
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
    assert_eq!(appended.status(), StatusCode::CREATED);

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
    let bytes = to_bytes(download.into_body(), usize::MAX)
        .await
        .expect("download body can be read");
    assert_eq!(&bytes[..], b"hello attachment");
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
