use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use hermes_hub_backend::{
    asr::{AsrClient, AsrError, AsrTranscription, AsrTranscriptionInput},
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::NoopDockerRuntime,
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::ModelRegistry,
    session::store::{SessionStore, SpeechInputSettings, SystemSettings},
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedAsrCall {
    file_name: String,
    content_type: String,
    bytes: Vec<u8>,
    file_path: PathBuf,
    language: Option<String>,
}

#[derive(Clone)]
struct RecordingAsrClient {
    calls: Arc<Mutex<Vec<RecordedAsrCall>>>,
    response: Arc<Mutex<Result<AsrTranscription, AsrError>>>,
}

impl Default for RecordingAsrClient {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: Arc::new(Mutex::new(Ok(AsrTranscription {
                text: "transcribed speech".to_string(),
            }))),
        }
    }
}

#[async_trait::async_trait]
impl AsrClient for RecordingAsrClient {
    async fn transcribe(&self, input: AsrTranscriptionInput) -> Result<AsrTranscription, AsrError> {
        let bytes = tokio::fs::read(&input.file_path)
            .await
            .map_err(|error| AsrError::RequestFailed(error.to_string()))?;
        self.calls
            .lock()
            .expect("calls lock")
            .push(RecordedAsrCall {
                file_name: input.file_name,
                content_type: input.content_type,
                bytes,
                file_path: input.file_path,
                language: input.language,
            });
        self.response.lock().expect("response lock").clone()
    }
}

fn speech_enabled_config() -> AppConfig {
    let mut config = AppConfig::for_tests();
    config.speech_input.enabled = true;
    config.speech_input.asr_endpoint = Some("http://asr:8090".to_string());
    config.speech_input.max_upload_bytes = 64;
    config
}

fn app_with_asr(config: AppConfig, asr_client: RecordingAsrClient) -> (Router, SessionStore) {
    let object_storage = InMemoryObjectStorage::new(config.object_storage.bucket.clone()).shared();
    let docker_provisioner = hermes_hub_backend::hermes::docker_provisioner::DockerProvisioner::new_with_runtime_and_object_storage(
        docker_config_from_app(&config, &config.initial_model_config),
        Arc::new(NoopDockerRuntime),
        object_storage.clone(),
    );
    let store = SessionStore::default();
    let state = AppState {
        model_registry: ModelRegistry::new(config.initial_model_config.clone()),
        config,
        store: store.clone(),
        channel_store: ChannelStore::default(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: hermes_hub_backend::channel::events::SessionEventHub::default(),
        asr_client: Arc::new(asr_client),
    };
    (build_router_with_state(state), store)
}

#[tokio::test]
async fn speech_input_config_requires_env_and_system_setting() {
    let asr_client = RecordingAsrClient::default();
    let (app, store) = app_with_asr(speech_enabled_config(), asr_client);
    let admin_cookie = bootstrap_admin(&app).await;

    let disabled_by_soft_switch = request_empty(
        &app,
        Method::GET,
        "/api/speech-input/config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(disabled_by_soft_switch).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["speech_input"]["enabled"], false);
    assert_eq!(body["speech_input"]["runtime_available"], true);

    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let enabled = request_empty(
        &app,
        Method::GET,
        "/api/speech-input/config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(enabled).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["speech_input"]["enabled"], true);
    assert_eq!(body["speech_input"]["runtime_available"], true);
    assert_eq!(body["speech_input"]["max_audio_seconds"], 60);
    assert_eq!(body["speech_input"]["max_upload_bytes"], 64);

    let (hard_disabled_app, hard_disabled_store) =
        app_with_asr(AppConfig::for_tests(), RecordingAsrClient::default());
    let admin_cookie = bootstrap_admin(&hard_disabled_app).await;
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    hard_disabled_store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let hard_disabled = request_empty(
        &hard_disabled_app,
        Method::GET,
        "/api/speech-input/config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(hard_disabled).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["speech_input"]["enabled"], false);
    assert_eq!(body["speech_input"]["runtime_available"], false);
}

#[tokio::test]
async fn speech_input_transcribes_one_recorded_audio_file() {
    let asr_client = RecordingAsrClient::default();
    *asr_client.response.lock().expect("response lock") = Ok(AsrTranscription {
        text: "你好 Hermes".to_string(),
    });
    let (app, store) = app_with_asr(speech_enabled_config(), asr_client.clone());
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let admin_cookie = bootstrap_admin(&app).await;

    let boundary = "speech-input-boundary";
    let mut body = Vec::new();
    multipart_file(
        &mut body,
        boundary,
        "file",
        "recording.webm",
        "audio/webm",
        b"voice bytes",
    );
    multipart_text(&mut body, boundary, "language", "zh");
    finish_multipart(&mut body, boundary);

    let response = request_raw(
        &app,
        Method::POST,
        "/api/speech-input/transcriptions",
        &format!("multipart/form-data; boundary={boundary}"),
        body,
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "你好 Hermes");

    let calls = asr_client.calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].file_name, "recording.webm");
    assert_eq!(calls[0].content_type, "audio/webm");
    assert_eq!(calls[0].bytes, b"voice bytes");
    assert_eq!(calls[0].language.as_deref(), Some("zh"));
    assert!(
        !calls[0].file_path.exists(),
        "speech temp file should be deleted after successful ASR"
    );
    drop(calls);

    let boundary = "speech-input-language-first-boundary";
    let mut body = Vec::new();
    multipart_text(&mut body, boundary, "language", "en");
    multipart_file(
        &mut body,
        boundary,
        "file",
        "recording.webm",
        "audio/webm",
        b"voice bytes again",
    );
    finish_multipart(&mut body, boundary);

    let response = request_raw(
        &app,
        Method::POST,
        "/api/speech-input/transcriptions",
        &format!("multipart/form-data; boundary={boundary}"),
        body,
        Some(&admin_cookie),
    )
    .await;
    let (status, _) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);

    let calls = asr_client.calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].language.as_deref(), Some("en"));
    assert!(
        !calls[1].file_path.exists(),
        "speech temp file should be deleted when language appears before file"
    );
}

#[tokio::test]
async fn speech_input_rejects_disabled_and_oversized_audio_without_calling_asr() {
    let asr_client = RecordingAsrClient::default();
    let (app, store) = app_with_asr(speech_enabled_config(), asr_client.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let disabled = speech_upload(&app, &admin_cookie, b"voice").await;
    assert_eq!(disabled.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(asr_client.calls.lock().expect("calls lock").is_empty());

    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let too_large = speech_upload(&app, &admin_cookie, &[b'x'; 65]).await;
    let (status, body) = response_json(too_large).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "attachment is too large");
    assert!(asr_client.calls.lock().expect("calls lock").is_empty());
}

#[tokio::test]
async fn speech_input_cleans_temp_file_after_asr_failure() {
    let asr_client = RecordingAsrClient::default();
    *asr_client.response.lock().expect("response lock") = Err(AsrError::Timeout);
    let (app, store) = app_with_asr(speech_enabled_config(), asr_client.clone());
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let admin_cookie = bootstrap_admin(&app).await;

    let response = speech_upload(&app, &admin_cookie, b"voice").await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

    let calls = asr_client.calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 1);
    assert!(
        !calls[0].file_path.exists(),
        "speech temp file should be deleted after failed ASR"
    );
}

async fn speech_upload(app: &Router, cookie: &str, payload: &[u8]) -> axum::response::Response {
    let boundary = "speech-input-limit-boundary";
    let mut body = Vec::new();
    multipart_file(
        &mut body,
        boundary,
        "file",
        "recording.webm",
        "audio/webm",
        payload,
    );
    finish_multipart(&mut body, boundary);
    request_raw(
        app,
        Method::POST,
        "/api/speech-input/transcriptions",
        &format!("multipart/form-data; boundary={boundary}"),
        body,
        Some(cookie),
    )
    .await
}

async fn request_empty(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
) -> axum::response::Response {
    request_raw(app, method, uri, "application/json", Vec::new(), cookie).await
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
) -> axum::response::Response {
    request_raw(
        app,
        method,
        uri,
        "application/json",
        serde_json::to_vec(&body).expect("json body"),
        cookie,
    )
    .await
}

async fn request_raw(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    cookie: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if !content_type.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    app.clone()
        .oneshot(builder.body(Body::from(body)).expect("request body"))
        .await
        .expect("request")
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).expect("json body")
        },
    )
}

fn multipart_file(
    body: &mut Vec<u8>,
    boundary: &str,
    field_name: &str,
    file_name: &str,
    content_type: &str,
    content: &[u8],
) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{file_name}\"\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(content);
    body.extend_from_slice(b"\r\n");
}

fn multipart_text(body: &mut Vec<u8>, boundary: &str, field_name: &str, content: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{field_name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(content.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn finish_multipart(body: &mut Vec<u8>, boundary: &str) {
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
}

async fn bootstrap_admin(app: &Router) -> String {
    let response = request_json(
        app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("cookie")
        .split(';')
        .next()
        .expect("session cookie")
        .to_string()
}
