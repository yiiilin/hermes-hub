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
    ldap::DefaultLdapAuthenticator,
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::{
        HermesScheduledTaskSnapshot, HermesSchedulerSnapshotInput, SessionStore, SystemSettings,
    },
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::{fs, sync::Arc};
use tempfile::tempdir;
use tower::ServiceExt;

fn test_state(store: SessionStore) -> AppState {
    test_state_with_channel_store(store, ChannelStore::default())
}

fn test_state_with_channel_store(store: SessionStore, channel_store: ChannelStore) -> AppState {
    let config = AppConfig::for_tests();
    let asr_client = hermes_hub_backend::asr::default_asr_client(&config.speech_input);
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
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
        asr_client,
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
        fallback: None,
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
        last_user_activity_at: None,
        last_started_at: None,
        last_stopped_at: None,
        stopped_reason: None,
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
async fn deleting_channel_session_removes_cron_jobs_targeting_that_session() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

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

    let temp = tempdir().expect("temp config dir can be created");
    let config_path = temp.path().join("config");
    let cron_path = config_path.join("cron");
    fs::create_dir_all(cron_path.join("output/task-for-deleted-session"))
        .expect("cron output dir can be created");
    fs::write(
        cron_path.join("jobs.json"),
        json!({
            "jobs": [
                {
                    "id": "task-for-deleted-session",
                    "name": "Deleted session task",
                    "origin": {
                        "platform": "hermes_hub",
                        "chat_id": session_id,
                        "thread_id": session_id
                    }
                },
                {
                    "id": "task-for-other-session",
                    "name": "Other task",
                    "origin": {
                        "platform": "hermes_hub",
                        "chat_id": "other-session",
                        "thread_id": "other-session"
                    }
                }
            ]
        })
        .to_string(),
    )
    .expect("jobs file can be written");

    let mut instance = managed_instance_for(&user_id);
    instance.host_config_path = Some(config_path.to_string_lossy().to_string());
    store
        .bind_hermes_instance(instance)
        .await
        .expect("instance can be rebound with temp config");

    let deleted = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/channels/{channel_id}/sessions/{session_id}"),
        Some(&cookie),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let jobs: Value = serde_json::from_str(
        &fs::read_to_string(cron_path.join("jobs.json")).expect("jobs file remains readable"),
    )
    .expect("jobs json remains valid");
    let job_names = jobs["jobs"]
        .as_array()
        .expect("jobs array")
        .iter()
        .map(|job| job["name"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(job_names, vec!["Other task"]);
    assert!(
        !cron_path.join("output/task-for-deleted-session").exists(),
        "deleted session cron output should be removed with the cron job"
    );
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
async fn public_session_events_stream_session_title_updates() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
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

    let session = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&cookie),
    )
    .await;
    let (_, session_body) = response_json(session).await;
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let stream_response = request_empty(
        &app,
        Method::GET,
        &format!("/api/sessions/{session_id}/events"),
        Some(&cookie),
    )
    .await;
    assert_eq!(stream_response.status(), StatusCode::OK);
    let mut body = stream_response.into_body().into_data_stream();
    let _snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
        .await
        .expect("snapshot event arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot chunk is readable");

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "标题实时刷新",
            "attachments": []
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let mut observed_events = String::new();
    for _ in 0..6 {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
            .await
            .expect("live event arrives")
            .expect("live chunk exists")
            .expect("live chunk is readable");
        observed_events.push_str(&String::from_utf8_lossy(&chunk));
        if observed_events.contains("session_updated") {
            assert!(observed_events.contains("标题实时刷新"));
            return;
        }
    }

    panic!("expected session_updated event, observed: {observed_events}");
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
async fn hermes_channel_protocol_delivers_atomic_media_output_message() {
    let store = SessionStore::default();
    store
        .update_system_settings(SystemSettings {
            max_attachment_upload_bytes: 4096,
            ..SystemSettings::default()
        })
        .await
        .expect("system attachment limit can be updated");
    let state = test_state(store.clone());
    let instance_token = "instance-channel-atomic-media-token";
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

    let boundary = "hermes-channel-atomic-media-boundary";
    let payload = vec![b'i'; 2048];
    fn atomic_media_body(boundary: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"caption\"\r\n\r\n\
             图片已生成\n\n{{{{attachment:0}}}}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"client_message_key\"\r\n\r\n\
             atomic-media-key\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"结果.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }
    let upload_body = atomic_media_body(boundary, &payload);

    let unauthorized = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body.clone(),
        None,
        None,
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, delivered_body) = response_json(delivered).await;
    assert_eq!(status, StatusCode::CREATED);
    let message = &delivered_body["message"];
    let message_id = message["id"].as_str().expect("message id");
    assert_eq!(message["content"], "图片已生成\n\n{{attachment:0}}");
    assert_eq!(message["attachments"][0]["direction"], "output");
    assert_eq!(message["attachments"][0]["kind"], "image");
    assert_eq!(message["attachments"][0]["name"], "结果.png");
    assert_eq!(message["attachments"][0]["content_type"], "image/png");
    assert_eq!(message["attachments"][0]["size"], payload.len());
    assert_eq!(message["attachments"][0]["message_id"], message_id);

    let download_url = message["attachments"][0]["download_url"]
        .as_str()
        .expect("download url");
    let download = request_empty(&app, Method::GET, download_url, Some(&cookie)).await;
    assert_eq!(download.status(), StatusCode::OK);
    let bytes = to_bytes(download.into_body(), usize::MAX)
        .await
        .expect("download body");
    assert_eq!(&bytes[..], payload.as_slice());

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
        messages_body["messages"]
            .as_array()
            .expect("messages")
            .len(),
        1
    );
    assert_eq!(messages_body["messages"][0]["id"], message_id);

    let repeated = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        atomic_media_body(boundary, b"second payload should be ignored"),
        None,
        Some(instance_token),
    )
    .await;
    let (status, repeated_body) = response_json(repeated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated_body["message"]["id"], message_id);
    let objects = state
        .object_storage
        .list_prefix("")
        .await
        .expect("objects can be listed");
    assert_eq!(objects.len(), 1);
}

#[tokio::test]
async fn hermes_channel_protocol_accepts_ordered_attachment_placeholders() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let instance_token = "instance-channel-ordered-attachments-token";
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

    let boundary = "hermes-channel-ordered-attachments-boundary";
    let script_payload = b"#!/bin/sh\n./start_ntp.sh\n";
    let image_payload = b"fake image bytes";
    let content = "脚本如下：\n\n{{attachment:0}}\n\n图片如下：\n\n{{attachment:1}}";
    let mut upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"content\"\r\n\r\n\
         {content}\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"client_message_key\"\r\n\r\n\
         ordered-attachments-key\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"start_ntp.sh\"\r\n\
         Content-Type: text/x-shellscript\r\n\r\n"
    )
    .into_bytes();
    upload_body.extend_from_slice(script_payload);
    upload_body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"结果.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    upload_body.extend_from_slice(image_payload);
    upload_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, delivered_body) = response_json(delivered).await;
    assert_eq!(status, StatusCode::CREATED);
    let message = &delivered_body["message"];
    let message_id = message["id"].as_str().expect("message id");
    assert_eq!(message["content"], content);
    assert_eq!(
        message["attachments"]
            .as_array()
            .expect("attachments")
            .len(),
        2
    );
    assert_eq!(message["attachments"][0]["name"], "start_ntp.sh");
    assert_eq!(message["attachments"][0]["kind"], "file");
    assert_eq!(message["attachments"][0]["message_id"], message_id);
    assert_eq!(message["attachments"][1]["name"], "结果.png");
    assert_eq!(message["attachments"][1]["kind"], "image");
    assert_eq!(message["attachments"][1]["message_id"], message_id);

    let script_download_url = message["attachments"][0]["download_url"]
        .as_str()
        .expect("script download url");
    let script_download =
        request_empty(&app, Method::GET, script_download_url, Some(&cookie)).await;
    assert_eq!(script_download.status(), StatusCode::OK);
    let script_bytes = to_bytes(script_download.into_body(), usize::MAX)
        .await
        .expect("script download body");
    assert_eq!(&script_bytes[..], script_payload);

    let repeated = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"content\"\r\n\r\n\
             {content}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"client_message_key\"\r\n\r\n\
             ordered-attachments-key\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"ignored.txt\"\r\n\
             Content-Type: text/plain\r\n\r\n\
             ignored\r\n\
             --{boundary}--\r\n"
        )
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, repeated_body) = response_json(repeated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated_body["message"]["id"], message_id);
}

#[tokio::test]
async fn hermes_channel_protocol_rejects_attachment_without_placeholder() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let instance_token = "instance-channel-placeholder-validation-token";
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

    let boundary = "hermes-channel-placeholder-validation-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"content\"\r\n\r\n\
         这里没有附件占位符\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"orphan.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         orphan\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(delivered.status(), StatusCode::BAD_REQUEST);

    let inline_upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"content\"\r\n\r\n\
         内联占位符 {{attachment:0}}\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"inline.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         inline\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let inline_delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        inline_upload_body,
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(inline_delivered.status(), StatusCode::BAD_REQUEST);

    let large_content = "x".repeat(2 * 1024 * 1024 + 1);
    let oversized_content_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"content\"\r\n\r\n\
         {large_content}\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"large-content.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         ignored\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let oversized_content = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        oversized_content_body,
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(oversized_content.status(), StatusCode::BAD_REQUEST);

    let large_unknown = "x".repeat(64 * 1024 + 1);
    let oversized_unknown_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"ignored\"\r\n\r\n\
         {large_unknown}\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let oversized_unknown = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        oversized_unknown_body,
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(oversized_unknown.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hermes_channel_protocol_returns_existing_media_output_after_run_stops() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let instance_token = "instance-channel-media-retry-token";
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
            "content": "生成一张图",
            "client_message_key": "media-retry-user-turn"
        }),
        Some(&cookie),
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = created_run_body["run"]["run_id"].as_str().expect("run id");
    let client_message_key = format!("hermes-run:{run_id}:media:1");
    let boundary = "hermes-channel-media-retry-boundary";

    fn media_retry_body(
        boundary: &str,
        run_id: &str,
        client_message_key: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"caption\"\r\n\r\n\
             结果图\n\n{{{{attachment:0}}}}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"run_id\"\r\n\r\n\
             {run_id}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"client_message_key\"\r\n\r\n\
             {client_message_key}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"retry.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    let delivered = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        media_retry_body(boundary, run_id, &client_message_key, b"first image"),
        None,
        Some(instance_token),
    )
    .await;
    let (status, delivered_body) = response_json(delivered).await;
    assert_eq!(status, StatusCode::CREATED);
    let message_id = delivered_body["message"]["id"]
        .as_str()
        .expect("message id");

    let stopped = request_empty(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/active-run/stop"),
        Some(&cookie),
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::OK);

    let repeated = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        media_retry_body(
            boundary,
            run_id,
            &client_message_key,
            b"late retry should be ignored",
        ),
        None,
        Some(instance_token),
    )
    .await;
    let (status, repeated_body) = response_json(repeated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated_body["message"]["id"], message_id);

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
        messages_body["messages"]
            .as_array()
            .expect("messages")
            .len(),
        2
    );
    let objects = state
        .object_storage
        .list_prefix("")
        .await
        .expect("objects can be listed");
    assert_eq!(objects.len(), 1);
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
    // 真实 PPT 回归约 12MB；这里用超过 Axum 默认 2MB、低于系统参数上限的载荷覆盖路由体限制。
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
async fn hermes_channel_protocol_rejects_output_attachment_over_system_upload_limit() {
    let store = SessionStore::default();
    store
        .update_system_settings(SystemSettings {
            max_attachment_upload_bytes: 1024,
            ..SystemSettings::default()
        })
        .await
        .expect("system attachment limit can be updated");
    let state = test_state(store.clone());
    let instance_token = "instance-channel-too-large-file-token";
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

    let boundary = "hermes-channel-too-large-file-boundary";
    let mut upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"too-large.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    upload_body.extend_from_slice(&vec![b'a'; 2048]);
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
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "attachment is too large");
}

#[tokio::test]
async fn hermes_channel_protocol_rejects_media_output_file_over_system_upload_limit() {
    let store = SessionStore::default();
    store
        .update_system_settings(SystemSettings {
            max_attachment_upload_bytes: 1024,
            ..SystemSettings::default()
        })
        .await
        .expect("system attachment limit can be updated");
    let state = test_state(store.clone());
    let instance_token = "instance-channel-too-large-media-token";
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

    let boundary = "hermes-channel-too-large-media-boundary";
    let mut upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"content\"\r\n\r\n\
         文件如下\n\n{{{{attachment:0}}}}\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"too-large.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    upload_body.extend_from_slice(&vec![b'a'; 2048]);
    upload_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/outputs/media"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        None,
        Some(instance_token),
    )
    .await;
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "attachment is too large");
    let objects = state
        .object_storage
        .list_prefix("")
        .await
        .expect("objects can be listed");
    assert!(objects.is_empty());
}

#[tokio::test]
async fn user_attachment_upload_rejects_file_over_system_upload_limit() {
    let store = SessionStore::default();
    store
        .update_system_settings(SystemSettings {
            max_attachment_upload_bytes: 1024,
            ..SystemSettings::default()
        })
        .await
        .expect("system attachment limit can be updated");
    let app = test_app(store.clone());
    let cookie = bootstrap_and_login(&app).await;

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

    let boundary = "user-too-large-file-boundary";
    let mut upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"too-large.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    upload_body.extend_from_slice(&vec![b'a'; 2048]);
    upload_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

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
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "attachment is too large");
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
async fn channel_inbox_delivers_gateway_restart_control_once() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    let instance_token = "instance-restart-control-token";
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .await
        .expect("instance can be bound");
    store
        .request_hermes_gateway_restart("instance-1")
        .await
        .expect("restart control can be queued");
    state
        .model_registry
        .add_instance_token_for_instance("instance-1", instance_token)
        .await
        .expect("instance token can be registered");

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
    let items = inbox_body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["type"], "control");
    assert_eq!(items[0]["action"], "restart_gateway");
    assert_eq!(items[0]["id"], "control:restart_gateway:instance-1");

    // 控制项只用于触发一次 gateway 重启，不能在容器重连后反复下发。
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
    assert_eq!(inbox_body["items"].as_array().expect("items").len(), 0);
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
async fn hermes_adapter_can_report_scheduler_snapshot_to_admin_view() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let app = build_router_with_state(state.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;
    let instance_token = "instance-scheduler-snapshot-token";
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
        json!({
            "scheduler_snapshot": {
                "status": "ok",
                "scheduler_enabled": true,
                "running_jobs_count": 1,
                "generated_at": 1_735_689_600,
                "source": "cron.jobs",
                "snapshot_hash": "snapshot-hash-1",
                "next_wake_at": 1_735_722_000,
                "jobs": [
                    {
                        "id": "task-daily-summary",
                        "name": "Daily summary",
                        "enabled": true,
                        "schedule": "0 9 * * *",
                        "timezone": "Asia/Shanghai",
                        "next_run_at": 1_735_722_000,
                        "last_run_at": 1_735_635_600,
                        "status": "scheduled",
                        "source": "hermes-adapter"
                    }
                ]
            }
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    let (status, body) = response_json(reported).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["id"], "instance-1");

    let snapshots = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-scheduler-snapshots",
        Some(&cookie),
    )
    .await;
    let (status, body) = response_json(snapshots).await;
    assert_eq!(status, StatusCode::OK);
    let snapshot = &body["hermes_scheduler_snapshots"][0];
    assert_eq!(snapshot["user_id"], user_id);
    assert_eq!(snapshot["user_email"], "admin@example.com");
    assert_eq!(snapshot["hermes_instance_id"], "instance-1");
    assert_eq!(snapshot["scheduler_enabled"], true);
    assert_eq!(snapshot["running_jobs_count"], 1);
    assert_eq!(snapshot["reported_at"], 1_735_689_600);
    assert_eq!(snapshot["tasks"][0]["id"], "task-daily-summary");
    assert_eq!(snapshot["tasks"][0]["name"], "Daily summary");
    assert_eq!(snapshot["tasks"][0]["schedule"], "0 9 * * *");
}

#[tokio::test]
async fn user_can_read_only_their_own_scheduler_snapshot() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_and_login(&app).await;
    let admin_id = store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin can be read from session")
        .id;

    let invite = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": 4_102_444_800u64,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, invite_body) = response_json(invite).await;
    assert_eq!(status, StatusCode::CREATED);
    let token = invite_body["token"].as_str().expect("invite token");
    let registered = request_json(
        &app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": token,
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(registered.status(), StatusCode::CREATED);
    let login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
    )
    .await;
    let user_cookie = cookie_from(&login);
    let user_id = store
        .user_by_session_cookie(&user_cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session")
        .id;

    let mut admin_instance = managed_instance_for(&admin_id);
    admin_instance.id = "instance-admin".to_string();
    let mut user_instance = managed_instance_for(&user_id);
    user_instance.id = "instance-user".to_string();
    user_instance.name = "hermes-user-regular".to_string();
    store
        .bind_hermes_instance(admin_instance)
        .await
        .expect("admin instance can be bound");
    store
        .bind_hermes_instance(user_instance)
        .await
        .expect("user instance can be bound");

    store
        .record_hermes_scheduler_snapshot(
            "instance-admin",
            HermesSchedulerSnapshotInput {
                scheduler_status: "ok".to_string(),
                scheduler_enabled: true,
                running_jobs_count: 0,
                reported_at: 1_735_689_600,
                source: "admin-scheduler".to_string(),
                snapshot_hash: Some("admin-hash".to_string()),
                next_wake_at: None,
                tasks: vec![HermesScheduledTaskSnapshot {
                    id: "task-admin".to_string(),
                    name: "Admin task".to_string(),
                    enabled: true,
                    schedule: "0 8 * * *".to_string(),
                    timezone: "Asia/Shanghai".to_string(),
                    next_run_at: None,
                    last_run_at: None,
                    status: "scheduled".to_string(),
                    source: "hermes-adapter".to_string(),
                }],
            },
        )
        .await
        .expect("admin snapshot can be stored");
    store
        .record_hermes_scheduler_snapshot(
            "instance-user",
            HermesSchedulerSnapshotInput {
                scheduler_status: "ok".to_string(),
                scheduler_enabled: true,
                running_jobs_count: 1,
                reported_at: 1_735_689_700,
                source: "user-scheduler".to_string(),
                snapshot_hash: Some("user-hash".to_string()),
                next_wake_at: Some(1_735_722_000),
                tasks: vec![HermesScheduledTaskSnapshot {
                    id: "task-user-daily".to_string(),
                    name: "User daily task".to_string(),
                    enabled: true,
                    schedule: "0 9 * * *".to_string(),
                    timezone: "Asia/Shanghai".to_string(),
                    next_run_at: Some(1_735_722_000),
                    last_run_at: Some(1_735_635_600),
                    status: "scheduled".to_string(),
                    source: "hermes-adapter".to_string(),
                }],
            },
        )
        .await
        .expect("user snapshot can be stored");

    let response = request_empty(
        &app,
        Method::GET,
        "/api/workspace/hermes-scheduler-snapshot",
        Some(&user_cookie),
    )
    .await;
    let (status, body) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    let snapshot = &body["hermes_scheduler_snapshot"];
    assert_eq!(snapshot["user_id"], user_id);
    assert_eq!(snapshot["hermes_instance_id"], "instance-user");
    assert_eq!(snapshot["tasks"][0]["name"], "User daily task");
    assert_eq!(snapshot["tasks"][0]["schedule"], "0 9 * * *");
    assert_ne!(snapshot["tasks"][0]["name"], "Admin task");
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
async fn adapter_execution_edit_after_newer_message_appends_to_latest_execution_slot() {
    let store = SessionStore::default();
    let state = test_state(store.clone());
    let instance_token = "instance-adapter-execution-order-token";
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

    let progress = request_raw(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/messages"),
        "application/json",
        json!({
            "role": "assistant",
            "content": "💻 terminal(['command'])\n{\"command\":\"first\"}",
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
        .expect("progress message id");
    assert_eq!(progress_body["message"]["message_kind"], "execution");

    let user_followup = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "继续",
            "attachments": []
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(user_followup.status(), StatusCode::CREATED);

    let moved_progress = request_raw(
        &app,
        Method::PUT,
        &format!("/internal/channel/v1/sessions/{session_id}/messages/{progress_message_id}"),
        "application/json",
        json!({
            "content": "💻 terminal(['command'])\n{\"command\":\"first\"}\n✅ terminal completed",
            "attachments": []
        })
        .to_string()
        .into_bytes(),
        None,
        Some(instance_token),
    )
    .await;
    assert_eq!(moved_progress.status(), StatusCode::CREATED);
    let (_, moved_progress_body) = response_json(moved_progress).await;
    assert_ne!(moved_progress_body["message"]["id"], progress_message_id);
    assert_eq!(moved_progress_body["message"]["message_kind"], "execution");

    let messages = request_empty(
        &app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&cookie),
    )
    .await;
    let (_, messages_body) = response_json(messages).await;
    let messages = messages_body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["id"], progress_message_id);
    assert_eq!(messages[0]["message_kind"], "execution");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["message_kind"], "text");
    assert_eq!(messages[2]["message_kind"], "execution");
    assert_eq!(
        messages[2]["content"],
        "💻 terminal(['command'])\n{\"command\":\"first\"}\n✅ terminal completed"
    );
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
