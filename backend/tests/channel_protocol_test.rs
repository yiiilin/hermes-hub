use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::{ChannelMessageRole, ChannelStore},
    docker_config_from_app,
    hermes::{
        docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    },
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn test_state(store: SessionStore) -> AppState {
    test_state_with_channel_store(store, ChannelStore::default())
}

fn test_state_with_channel_store(store: SessionStore, channel_store: ChannelStore) -> AppState {
    let config = AppConfig::for_tests();
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store,
        channel_store,
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
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
    })
}

fn test_app(store: SessionStore) -> Router {
    build_router_with_state(test_state(store))
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
        api_token_secret_ref: Some("hermes-secret-token".to_string()),
        llm_api_key: None,
        container_id: Some("container-1".to_string()),
        host_workspace_path: Some("/tmp/hermes/admin/workspace".to_string()),
        host_sandbox_path: Some("/tmp/hermes/admin/sandbox".to_string()),
        host_config_path: Some("/tmp/hermes/admin/config".to_string()),
        health_status: "healthy".to_string(),
        status_message: None,
        runtime_image: Some("ghcr.io/yiiilin/hermes-hub-hermes:1.2.3".to_string()),
        runtime_version: Some("1.2.3".to_string()),
        global_skills_write_enabled: false,
    }
}

#[tokio::test]
async fn channel_messages_and_attachments_are_hub_owned() {
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let app = test_app(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
async fn hermes_adapter_can_report_runtime_version_to_hub() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    let instance_token = "instance-runtime-version-token";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");

    let reported = request_raw(
        &app,
        Method::POST,
        "/internal/channel/v1/instance/status",
        "application/json",
        br#"{"runtime_version":"0.13.7"}"#.to_vec(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, body) = response_json(reported).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["runtime_version"], "0.13.7");
    let stored = store
        .hermes_instance_for_user(&user_id)
        .await
        .expect("reported runtime version is persisted");
    assert_eq!(stored.runtime_version.as_deref(), Some("0.13.7"));
}

#[tokio::test]
async fn channel_run_enqueue_can_be_polled_and_completed_by_hermes_hub_adapter() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
    let store = SessionStore::default();
    let state = test_state(store.clone());
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
