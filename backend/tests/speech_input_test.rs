use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use hermes_hub_backend::{
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
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest, Message},
};
use tower::ServiceExt;

fn speech_enabled_config() -> AppConfig {
    let mut config = AppConfig::for_tests();
    config.speech_input.enabled = true;
    config.speech_input.asr_endpoint = Some("http://asr:9991".to_string());
    config.speech_input.max_audio_seconds = 45;
    config
}

fn speech_enabled_config_with_endpoint(endpoint: String) -> AppConfig {
    let mut config = speech_enabled_config();
    config.speech_input.asr_endpoint = Some(endpoint);
    config
}

fn app_with_config(config: AppConfig) -> (Router, SessionStore) {
    let object_storage = InMemoryObjectStorage::new(config.object_storage.bucket.clone()).shared();
    let docker_provisioner =
        hermes_hub_backend::hermes::docker_provisioner::DockerProvisioner::new_with_runtime_and_object_storage(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
            object_storage.clone(),
        );
    let store = SessionStore::in_memory_for_tests();
    let state = AppState {
        model_registry: ModelRegistry::in_memory_for_tests(config.initial_model_config.clone()),
        config,
        store: store.clone(),
        channel_store: ChannelStore::in_memory_for_tests(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: hermes_hub_backend::channel::events::SessionEventHub::default(),
    };
    (build_router_with_state(state), store)
}

#[tokio::test]
async fn speech_input_config_exposes_streaming_contract() {
    let (app, store) = app_with_config(speech_enabled_config());
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
    assert_eq!(body["speech_input"]["max_duration_seconds"], 45);
    assert_eq!(body["speech_input"]["sample_rate"], 16000);
    assert!(body["speech_input"].get("max_upload_bytes").is_none());

    let (hard_disabled_app, hard_disabled_store) = app_with_config(AppConfig::for_tests());
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
async fn speech_input_transcriptions_route_is_removed() {
    let (app, store) = app_with_config(speech_enabled_config());
    let admin_cookie = bootstrap_admin(&app).await;
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");

    let response = request_raw(
        &app,
        Method::POST,
        "/api/speech-input/transcriptions",
        "application/octet-stream",
        b"voice".to_vec(),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn speech_input_stream_requires_login() {
    let (app, _) = app_with_config(speech_enabled_config());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });

    let result = connect_async(format!("ws://{addr}/api/speech-input/stream")).await;
    match result {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected unauthorized websocket handshake, got {other:?}"),
    }
}

#[tokio::test]
async fn speech_input_stream_proxies_asr_messages() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(message)) = socket.recv().await {
                        match message {
                            axum::extract::ws::Message::Text(text)
                                if text.contains("\"type\":\"start\"") =>
                            {
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        r#"{"type":"partial","text":"你"}"#.into(),
                                    ))
                                    .await;
                            }
                            axum::extract::ws::Message::Text(text)
                                if text.contains("\"type\":\"stop\"") =>
                            {
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        r#"{"type":"final","text":"你好"}"#.into(),
                                    ))
                                    .await;
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        r#"{"type":"done"}"#.into(),
                                    ))
                                    .await;
                                break;
                            }
                            _ => {}
                        }
                    }
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let (app, store) = app_with_config(speech_enabled_config_with_endpoint(format!(
        "http://{asr_addr}"
    )));
    let admin_cookie = bootstrap_admin(&app).await;
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
    let hub_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind hub");
    let hub_addr = hub_listener.local_addr().expect("hub addr");
    tokio::spawn(async move {
        axum::serve(hub_listener, app).await.expect("serve hub");
    });

    let mut request = format!("ws://{hub_addr}/api/speech-input/stream")
        .into_client_request()
        .expect("websocket request");
    request
        .headers_mut()
        .insert(header::COOKIE, admin_cookie.parse().expect("cookie header"));
    let (mut socket, _) = connect_async(request).await.expect("connect hub stream");

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");
    socket
        .send(Message::Binary(vec![1, 2, 3, 4].into()))
        .await
        .expect("send audio");
    socket
        .send(Message::Text(r#"{"type":"stop"}"#.into()))
        .await
        .expect("send stop");

    let partial = socket.next().await.expect("partial").expect("partial ok");
    let final_message = socket.next().await.expect("final").expect("final ok");
    let done = socket.next().await.expect("done").expect("done ok");

    assert_eq!(
        partial.to_text().expect("partial text"),
        r#"{"type":"partial","text":"你"}"#
    );
    assert_eq!(
        final_message.to_text().expect("final text"),
        r#"{"type":"final","text":"你好"}"#
    );
    assert_eq!(done.to_text().expect("done text"), r#"{"type":"done"}"#);
}

#[tokio::test]
async fn speech_input_stream_forwards_asr_error_messages() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    let _ = socket.recv().await;
                    let _ = socket
                        .send(axum::extract::ws::Message::Text(
                            r#"{"type":"error","message":"asr failed"}"#.into(),
                        ))
                        .await;
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let (app, store) = app_with_config(speech_enabled_config_with_endpoint(format!(
        "http://{asr_addr}"
    )));
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");

    let error = socket.next().await.expect("error").expect("error ok");
    assert_eq!(
        error.to_text().expect("error text"),
        r#"{"type":"error","message":"asr failed"}"#
    );
}

#[tokio::test]
async fn speech_input_stream_closes_at_configured_max_duration() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    while socket.recv().await.is_some() {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let mut config = speech_enabled_config_with_endpoint(format!("http://{asr_addr}"));
    config.speech_input.max_audio_seconds = 1;
    let (app, store) = app_with_config(config);
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");

    let error = socket
        .next()
        .await
        .expect("duration error")
        .expect("error ok");
    assert_eq!(
        error.to_text().expect("error text"),
        r#"{"type":"error","message":"speech input exceeded max duration"}"#
    );
}

#[tokio::test]
async fn speech_input_stream_allows_asr_finalization_after_stop() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(message)) = socket.recv().await {
                        if let axum::extract::ws::Message::Text(text) = message {
                            if text.contains("\"type\":\"stop\"") {
                                tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        r#"{"type":"final","text":"慢速最终结果"}"#.into(),
                                    ))
                                    .await;
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        r#"{"type":"done"}"#.into(),
                                    ))
                                    .await;
                                break;
                            }
                        }
                    }
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let mut config = speech_enabled_config_with_endpoint(format!("http://{asr_addr}"));
    config.speech_input.max_audio_seconds = 1;
    let (app, store) = app_with_config(config);
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");
    socket
        .send(Message::Text(r#"{"type":"stop"}"#.into()))
        .await
        .expect("send stop");

    let final_message = socket.next().await.expect("final").expect("final ok");
    let done = socket.next().await.expect("done").expect("done ok");
    assert_eq!(
        final_message.to_text().expect("final text"),
        r#"{"type":"final","text":"慢速最终结果"}"#
    );
    assert_eq!(done.to_text().expect("done text"), r#"{"type":"done"}"#);
}

#[tokio::test]
async fn speech_input_stream_times_out_asr_finalization_after_stop() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(message)) = socket.recv().await {
                        if let axum::extract::ws::Message::Text(text) = message {
                            if text.contains("\"type\":\"stop\"") {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                break;
                            }
                        }
                    }
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let mut config = speech_enabled_config_with_endpoint(format!("http://{asr_addr}"));
    config.speech_input.timeout_seconds = 1;
    let (app, store) = app_with_config(config);
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");
    socket
        .send(Message::Text(r#"{"type":"stop"}"#.into()))
        .await
        .expect("send stop");

    let error = socket
        .next()
        .await
        .expect("finalization timeout")
        .expect("error ok");
    assert_eq!(
        error.to_text().expect("error text"),
        r#"{"type":"error","message":"asr stream finalization timed out"}"#
    );
}

#[tokio::test]
async fn speech_input_stream_rejects_audio_over_total_limit() {
    let asr_addr = spawn_draining_asr_server().await;
    let mut config = speech_enabled_config_with_endpoint(format!("http://{asr_addr}"));
    config.speech_input.max_audio_seconds = 1;
    let (app, store) = app_with_config(config);
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Binary(vec![0; 32_001].into()))
        .await
        .expect("send oversized audio");

    let error = socket
        .next()
        .await
        .expect("audio size error")
        .expect("error ok");
    assert_eq!(
        error.to_text().expect("error text"),
        r#"{"type":"error","message":"speech input exceeded max audio size"}"#
    );
}

#[tokio::test]
async fn speech_input_stream_rejects_large_single_audio_frame() {
    let asr_addr = spawn_draining_asr_server().await;
    let (app, store) = app_with_config(speech_enabled_config_with_endpoint(format!(
        "http://{asr_addr}"
    )));
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Binary(vec![0; 160_001].into()))
        .await
        .expect("send oversized frame");

    let error = socket
        .next()
        .await
        .expect("audio frame error")
        .expect("error ok");
    assert_eq!(
        error.to_text().expect("error text"),
        r#"{"type":"error","message":"speech input exceeded max audio size"}"#
    );
}

#[tokio::test]
async fn speech_input_stream_treats_asr_close_as_normal_close() {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    let _ = socket.recv().await;
                    let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
                })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });

    let (app, store) = app_with_config(speech_enabled_config_with_endpoint(format!(
        "http://{asr_addr}"
    )));
    let admin_cookie = bootstrap_admin(&app).await;
    enable_speech_input(&store).await;
    let (mut socket, _hub_addr) = connect_authenticated_speech_stream(app, admin_cookie).await;

    socket
        .send(Message::Text(
            r#"{"type":"start","sample_rate":16000}"#.into(),
        ))
        .await
        .expect("send start");

    let close = socket.next().await.expect("close").expect("close ok");
    assert!(matches!(close, Message::Close(_)));
}

async fn request_empty(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
) -> axum::response::Response {
    request_raw(app, method, uri, "application/json", Vec::new(), cookie).await
}

async fn enable_speech_input(store: &SessionStore) {
    let mut settings = SystemSettings::default();
    settings.speech_input = SpeechInputSettings { enabled: true };
    store
        .update_system_settings(settings)
        .await
        .expect("system settings can enable speech input");
}

async fn connect_authenticated_speech_stream(
    app: Router,
    admin_cookie: String,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    std::net::SocketAddr,
) {
    let hub_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind hub");
    let hub_addr = hub_listener.local_addr().expect("hub addr");
    tokio::spawn(async move {
        axum::serve(hub_listener, app).await.expect("serve hub");
    });
    let mut request = format!("ws://{hub_addr}/api/speech-input/stream")
        .into_client_request()
        .expect("websocket request");
    request
        .headers_mut()
        .insert(header::COOKIE, admin_cookie.parse().expect("cookie header"));
    let (socket, _) = connect_async(request).await.expect("connect hub stream");
    (socket, hub_addr)
}

async fn spawn_draining_asr_server() -> std::net::SocketAddr {
    let asr_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake asr");
    let asr_addr = asr_listener.local_addr().expect("fake asr addr");
    tokio::spawn(async move {
        let app = Router::new().route(
            "/stream",
            axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move { while socket.recv().await.is_some() {} })
            }),
        );
        axum::serve(asr_listener, app)
            .await
            .expect("serve fake asr");
    });
    asr_addr
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

async fn bootstrap_admin(app: &Router) -> String {
    let created = request_json(
        app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123",
        }),
        None,
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let response = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123",
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("set-cookie utf8")
        .split(';')
        .next()
        .expect("cookie value")
        .to_string()
}
