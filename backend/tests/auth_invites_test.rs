use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, Method, Request, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use hermes_hub_backend::{build_router, AppConfig};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::{DynLdapAuthenticator, InMemoryLdapAuthenticator},
    llm_proxy::InMemoryLlmProviderClient,
    model_config::ModelRegistry,
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppState,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn test_app() -> Router {
    build_router(AppConfig::for_tests())
}

fn test_app_with_ldap(ldap_authenticator: DynLdapAuthenticator) -> Router {
    let config = AppConfig::for_tests();
    let asr_client = hermes_hub_backend::asr::default_asr_client(&config.speech_input);
    let state = AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        model_registry: ModelRegistry::default_for_tests(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator,
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
        asr_client,
    };
    build_router_with_state(state)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
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

async fn request_empty_with_headers(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }

    app.clone()
        .oneshot(builder.body(Body::empty()).expect("request can be built"))
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

async fn bootstrap_admin(app: &Router) {
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
}

async fn login(app: &Router, email: &str, password: &str) -> String {
    let response = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": email,
            "password": password
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    cookie_from(&response)
}

#[tokio::test]
async fn authenticated_user_can_update_local_password() {
    let app = test_app();
    let unauthorized = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({
            "new_password": "new-password-456"
        }),
        None,
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let empty = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({
            "new_password": " "
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let update = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({
            "new_password": "new-password-456"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let old_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);

    let new_cookie = login(&app, "admin@example.com", "new-password-456").await;
    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&new_cookie)).await;
    assert_eq!(me.status(), StatusCode::OK);
}

#[tokio::test]
async fn ldap_auto_created_user_can_set_local_password_by_same_email() {
    let ldap = InMemoryLdapAuthenticator::default();
    ldap.add_user(
        "uid=ldap-user,ou=people,dc=example,dc=com",
        "ldap-user@example.com",
        "ldap-password-123",
    );
    let app = test_app_with_ldap(ldap.shared());
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let update_settings = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": { "enabled": false },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(mail={email})",
                "email_attribute": "mail",
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_settings.status(), StatusCode::NO_CONTENT);

    let ldap_login = request_json(
        &app,
        Method::POST,
        "/api/auth/ldap/login",
        json!({
            "email": "ldap-user@example.com",
            "password": "ldap-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(ldap_login.status(), StatusCode::OK);
    let ldap_cookie = cookie_from(&ldap_login);

    let password_update = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({
            "new_password": "local-password-456"
        }),
        Some(&ldap_cookie),
    )
    .await;
    assert_eq!(password_update.status(), StatusCode::NO_CONTENT);

    let local_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "ldap-user@example.com",
            "password": "local-password-456"
        }),
        None,
    )
    .await;
    assert_eq!(local_login.status(), StatusCode::OK);
}

#[tokio::test]
async fn public_platform_admin_status_and_rebuild_manage_the_public_hermes_instance() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let initial = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/hermes-instance",
        Some(&admin_cookie),
    )
    .await;
    let (status, initial_body) = response_json(initial).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial_body["enabled"], false);
    assert_eq!(initial_body["ready"], false);
    assert_eq!(initial_body["hermes_instance"], Value::Null);

    let disabled_rebuild = request_empty(
        &app,
        Method::POST,
        "/api/admin/public-platform/hermes-instance/rebuild",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(disabled_rebuild.status(), StatusCode::CONFLICT);

    let update_settings = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "public_platform": {
                "enabled": true,
                "temporary_session_retention_hours": 24
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_settings.status(), StatusCode::NO_CONTENT);

    let enabled_status = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/hermes-instance",
        Some(&admin_cookie),
    )
    .await;
    let (status, enabled_body) = response_json(enabled_status).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(enabled_body["enabled"], true);
    assert_eq!(enabled_body["ready"], true);
    assert_eq!(enabled_body["hermes_instance"]["status"], "running");
    assert_eq!(enabled_body["hermes_instance"]["health_status"], "running");
    let public_user_id = enabled_body["hermes_instance"]["user_id"]
        .as_str()
        .expect("public Hermes user id exists")
        .to_string();
    assert!(enabled_body["hermes_instance"]["host_sandbox_path"]
        .as_str()
        .expect("public Hermes has sandbox")
        .ends_with("/sandbox"));

    let ordinary_instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, ordinary_instances_body) = response_json(ordinary_instances).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ordinary_instances_body["hermes_instances"]
            .as_array()
            .expect("ordinary instance list")
            .iter()
            .all(|instance| instance["user_id"] != public_user_id),
        "public Hermes must stay out of the ordinary managed instance list"
    );

    let ordinary_stop = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{public_user_id}/hermes-instance/stop"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(ordinary_stop.status(), StatusCode::NOT_FOUND);
    let ordinary_disable = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{public_user_id}/disable"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(ordinary_disable.status(), StatusCode::NOT_FOUND);
    let ordinary_enable = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{public_user_id}/enable"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(ordinary_enable.status(), StatusCode::NOT_FOUND);

    let bootstrap = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, bootstrap_body) = response_json(bootstrap).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap_body["public_platform_enabled"], true);

    let rebuilt = request_empty(
        &app,
        Method::POST,
        "/api/admin/public-platform/hermes-instance/rebuild",
        Some(&admin_cookie),
    )
    .await;
    let (status, rebuilt_body) = response_json(rebuilt).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rebuilt_body["hermes_instance"]["status"], "running");
    assert!(rebuilt_body["hermes_instance"]["host_sandbox_path"]
        .as_str()
        .expect("rebuilt public Hermes has sandbox")
        .ends_with("/sandbox"));
}

#[tokio::test]
async fn anonymous_public_sessions_wait_until_public_hermes_is_ready() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update_settings = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "public_platform": {
                "enabled": true,
                "temporary_session_retention_hours": 24
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_settings.status(), StatusCode::NO_CONTENT);

    let bootstrap = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, bootstrap_body) = response_json(bootstrap).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap_body["public_platform_enabled"], false);

    let list_sessions = request_empty(&app, Method::GET, "/api/sessions", None).await;
    let (status, body) = response_json(list_sessions).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["message"], "public platform is not ready");

    let admin_public_sessions = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/sessions",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(admin_public_sessions).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
    assert_eq!(
        body["sessions"]
            .as_array()
            .expect("public sessions array")
            .len(),
        0
    );
}

#[tokio::test]
async fn admin_can_page_and_force_clear_public_platform_sessions() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let update_settings = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "public_platform": {
                "enabled": true,
                "temporary_session_retention_hours": 24
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_settings.status(), StatusCode::NO_CONTENT);

    for index in 0..12 {
        let created = request_json(
            &app,
            Method::POST,
            "/api/sessions",
            json!({
                "kind": "agent",
                "title": format!("public session {index}")
            }),
            None,
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let first_page = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/sessions?page=1&page_size=10",
        Some(&admin_cookie),
    )
    .await;
    let (status, first_page_body) = response_json(first_page).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_page_body["page"], 1);
    assert_eq!(first_page_body["page_size"], 10);
    assert_eq!(first_page_body["total"], 12);
    assert_eq!(first_page_body["total_pages"], 2);
    assert_eq!(
        first_page_body["sessions"]
            .as_array()
            .expect("first page sessions")
            .len(),
        10
    );
    assert!(first_page_body["sessions"][0]["public_url"]
        .as_str()
        .expect("public url exists")
        .starts_with("/public/sessions/"));
    assert!(
        first_page_body["sessions"][0]["recycle_at"]
            .as_u64()
            .expect("recycle_at exists")
            >= first_page_body["sessions"][0]["created_at"]
                .as_u64()
                .expect("created_at exists")
    );

    let second_page = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/sessions?page=2&page_size=10",
        Some(&admin_cookie),
    )
    .await;
    let (status, second_page_body) = response_json(second_page).await;
    assert_eq!(status, StatusCode::OK);
    let second_page_sessions = second_page_body["sessions"]
        .as_array()
        .expect("second page sessions");
    assert_eq!(second_page_sessions.len(), 2);
    let clear_session_id = second_page_sessions[0]["id"]
        .as_str()
        .expect("session id exists")
        .to_string();

    let cleared = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/admin/public-platform/sessions/{clear_session_id}"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);

    let after_clear = request_empty(
        &app,
        Method::GET,
        "/api/admin/public-platform/sessions?page=2&page_size=10",
        Some(&admin_cookie),
    )
    .await;
    let (status, after_clear_body) = response_json(after_clear).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after_clear_body["total"], 11);
    assert_eq!(
        after_clear_body["sessions"]
            .as_array()
            .expect("remaining second page sessions")
            .len(),
        1
    );

    let invalid_clear = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/public-platform/sessions/not-a-uuid",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(invalid_clear.status(), StatusCode::NOT_FOUND);

    let private_session = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({
            "kind": "agent",
            "title": "private admin session"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(private_session.status(), StatusCode::CREATED);
    let (_, private_body) = response_json(private_session).await;
    let private_session_id = private_body["session"]["id"]
        .as_str()
        .expect("private session id");
    let private_clear = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/admin/public-platform/sessions/{private_session_id}"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(private_clear.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oidc_start_redirects_with_configured_authorization_parameters() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_empty(&app, Method::GET, "/api/auth/oidc/start", None).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location is valid")
        .to_string();
    assert!(location.starts_with("https://idp.example.com/oauth2/v1/authorize?"));
    assert!(location.contains("client_id=hermes-hub"));
    assert!(location.contains("response_type=code"));
    assert!(location.contains("scope=openid%20profile%20email"));
    assert!(location.contains("redirect_uri=http%3A%2F%2Flocalhost%2Fapi%2Fauth%2Foidc%2Fcallback"));
    assert!(location.contains("state="));
    assert!(location.contains("nonce="));
}

#[tokio::test]
async fn oidc_start_uses_forwarded_origin_for_redirect_uri() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_empty_with_headers(
        &app,
        Method::GET,
        "/api/auth/oidc/start",
        &[("host", "hub.example.com"), ("x-forwarded-proto", "https")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location is valid");

    assert!(location
        .contains("redirect_uri=https%3A%2F%2Fhub.example.com%2Fapi%2Fauth%2Foidc%2Fcallback"));
}

#[tokio::test]
async fn oidc_password_login_stays_available_for_same_email_accounts() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": false,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let password_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(password_login.status(), StatusCode::OK);

    let public_config = request_empty(&app, Method::GET, "/api/auth/oidc/config", None).await;
    let (status, body) = response_json(public_config).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["oidc"]["allow_password_login"], true);
}

#[tokio::test]
async fn oidc_callback_exchanges_code_creates_user_and_sets_session_cookie() {
    let provider_base_url = spawn_oidc_provider_server().await;
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": format!("{provider_base_url}/authorize"),
                "token_url": format!("{provider_base_url}/token"),
                "userinfo_url": format!("{provider_base_url}/userinfo"),
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);
    configure_required_model_configs(&app, &admin_cookie).await;

    let start = request_empty(&app, Method::GET, "/api/auth/oidc/start", None).await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let state_cookie = cookie_from(&start);
    let state = state_cookie
        .split_once('=')
        .map(|(_, value)| value)
        .expect("state cookie has a value");

    let callback = request_empty(
        &app,
        Method::GET,
        &format!("/api/auth/oidc/callback?code=auth-code&state={state}"),
        Some(&state_cookie),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    assert_eq!(
        callback
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/")
    );
    let session_cookie = cookie_from(&callback);

    let password_update = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({
            "new_password": "local-password-456"
        }),
        Some(&session_cookie),
    )
    .await;
    assert_eq!(password_update.status(), StatusCode::NO_CONTENT);

    let local_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "oidc-user@example.com",
            "password": "local-password-456"
        }),
        None,
    )
    .await;
    assert_eq!(local_login.status(), StatusCode::OK);

    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&session_cookie)).await;
    let (status, body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "oidc-user@example.com");
    assert_eq!(body["user"]["role"], "user");
    let oidc_user_id = body["user"]["id"]
        .as_str()
        .expect("OIDC user id exists")
        .to_string();

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], oidc_user_id);
    assert_eq!(body["hermes_instances"][0]["kind"], "managed_docker");
    assert_eq!(body["hermes_instances"][0]["status"], "running");
}

#[tokio::test]
async fn oidc_login_links_existing_local_user_by_email() {
    let provider_base_url = spawn_oidc_provider_server().await;
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;
    let token = invite["token"].as_str().expect("token exists");
    let registered = redeem_invite(&app, token, "oidc-user@example.com").await;
    let (status, registered_body) = response_json(registered).await;
    assert_eq!(status, StatusCode::CREATED);
    let existing_user_id = registered_body["user"]["id"]
        .as_str()
        .expect("registered user id exists")
        .to_string();

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": format!("{provider_base_url}/authorize"),
                "token_url": format!("{provider_base_url}/token"),
                "userinfo_url": format!("{provider_base_url}/userinfo"),
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": false
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let start = request_empty(&app, Method::GET, "/api/auth/oidc/start", None).await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let state_cookie = cookie_from(&start);
    let state = state_cookie
        .split_once('=')
        .map(|(_, value)| value)
        .expect("state cookie has a value");

    let callback = request_empty(
        &app,
        Method::GET,
        &format!("/api/auth/oidc/callback?code=auth-code&state={state}"),
        Some(&state_cookie),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    let session_cookie = cookie_from(&callback);

    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&session_cookie)).await;
    let (status, body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["id"], existing_user_id);
    assert_eq!(body["user"]["email"], "oidc-user@example.com");
    assert_eq!(body["user"]["role"], "user");
}

#[tokio::test]
async fn ldap_login_links_existing_user_by_email() {
    let ldap = InMemoryLdapAuthenticator::default();
    ldap.add_user(
        "uid=admin,ou=people,dc=example,dc=com",
        "admin@example.com",
        "ldap-password-123",
    );
    let app = test_app_with_ldap(ldap.shared());
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": { "enabled": false },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(mail={email})",
                "email_attribute": "mail",
                "auto_create_users": false
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/ldap/login",
        json!({
            "email": "ADMIN@example.com",
            "password": "ldap-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_from(&response);
    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "admin@example.com");
    assert_eq!(body["user"]["role"], "admin");

    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    let (status, body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "admin@example.com");
}

#[tokio::test]
async fn ldap_login_auto_creates_user_by_email_and_ensures_hermes() {
    let ldap = InMemoryLdapAuthenticator::default();
    ldap.add_user(
        "uid=ldap-user,ou=people,dc=example,dc=com",
        "ldap-user@example.com",
        "ldap-password-123",
    );
    let app = test_app_with_ldap(ldap.shared());
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": { "enabled": false },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(mail={email})",
                "email_attribute": "mail",
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/ldap/login",
        json!({
            "email": "LDAP-USER@example.com",
            "password": "ldap-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_from(&response);
    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "ldap-user@example.com");
    assert_eq!(body["user"]["role"], "user");
    let ldap_user_id = body["user"]["id"]
        .as_str()
        .expect("LDAP user id exists")
        .to_string();

    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    let (status, body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "ldap-user@example.com");

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], ldap_user_id);
    assert_eq!(body["hermes_instances"][0]["kind"], "managed_docker");
    assert_eq!(body["hermes_instances"][0]["status"], "running");
}

#[tokio::test]
async fn ldap_login_rejects_new_user_when_auto_create_is_disabled() {
    let ldap = InMemoryLdapAuthenticator::default();
    ldap.add_user(
        "uid=ldap-user,ou=people,dc=example,dc=com",
        "ldap-user@example.com",
        "ldap-password-123",
    );
    let app = test_app_with_ldap(ldap.shared());
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": { "enabled": false },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(mail={email})",
                "email_attribute": "mail",
                "auto_create_users": false
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/ldap/login",
        json!({
            "email": "ldap-user@example.com",
            "password": "ldap-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

async fn create_invite(app: &Router, admin_cookie: &str, expires_at: u64, max_uses: u32) -> Value {
    let response = request_json(
        app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": expires_at,
            "max_uses": max_uses
        }),
        Some(admin_cookie),
    )
    .await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::CREATED);
    value
}

async fn spawn_oidc_provider_server() -> String {
    let app = Router::new()
        .route("/token", post(oidc_token_handler))
        .route("/userinfo", get(oidc_userinfo_handler));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test OIDC provider can bind");
    let addr = listener.local_addr().expect("test OIDC provider addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test OIDC provider server runs");
    });

    format!("http://{addr}")
}

async fn oidc_token_handler() -> Json<Value> {
    Json(json!({
        "access_token": "access-token",
        "token_type": "Bearer"
    }))
}

async fn oidc_userinfo_handler(headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-token")
    );
    Json(json!({
        "email": "oidc-user@example.com",
        "preferred_username": "oidc-user"
    }))
}

async fn configure_required_model_configs(app: &Router, admin_cookie: &str) {
    for (config_kind, model) in [("llm", "gpt-4.1-mini"), ("title", "gpt-4.1-mini")] {
        let response = request_json(
            app,
            Method::PUT,
            "/api/admin/model-config",
            json!({
                "config_kind": config_kind,
                "provider_name": "openai-compatible",
                "provider_base_url": "https://ready-provider.example/v1",
                "provider_api_key": "ready-provider-key",
                "default_model": model,
                "allowed_models": [model],
                "allow_streaming": config_kind == "llm",
                "request_timeout_seconds": 30
            }),
            Some(admin_cookie),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

async fn redeem_invite(app: &Router, token: &str, email: &str) -> Response<Body> {
    request_json(
        app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": token,
            "email": email,
            "password": "user-password-123"
        }),
        None,
    )
    .await
}

#[tokio::test]
async fn first_user_bootstrap_registers_admin_and_blocks_second_bootstrap() {
    let app = test_app();

    let response = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["bootstrap_open"], true);

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    let admin_cookie = cookie_from(&response);
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(value["user"]["email"], "admin@example.com");
    assert_eq!(value["user"]["role"], "admin");

    let response = request_empty(&app, Method::GET, "/api/auth/me", Some(&admin_cookie)).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["user"]["email"], "admin@example.com");

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "second-admin@example.com",
            "password": "admin-password-456"
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["bootstrap_open"], false);
}

#[tokio::test]
async fn password_hashing_accepts_right_password_and_rejects_wrong_password() {
    let hash = hermes_hub_backend::domain::user::hash_password("correct-password")
        .expect("password can be hashed");

    assert_ne!(hash, "correct-password");
    assert!(
        hermes_hub_backend::domain::user::verify_password(&hash, "correct-password")
            .expect("stored hash can be verified")
    );
    assert!(
        !hermes_hub_backend::domain::user::verify_password(&hash, "wrong-password")
            .expect("stored hash can reject mismatches")
    );
}

#[tokio::test]
async fn login_sets_cookie_me_reads_user_and_logout_clears_session() {
    let app = test_app();
    bootstrap_admin(&app).await;

    let cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let response = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["user"]["email"], "admin@example.com");
    assert_eq!(value["user"]["role"], "admin");

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "wrong-password"
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = request_empty(&app, Method::POST, "/api/auth/logout", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response
        .headers()
        .get(header::SET_COOKIE)
        .expect("logout clears cookie")
        .to_str()
        .expect("cookie is ascii")
        .contains("Max-Age=0"));

    let response = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invite_creation_requires_expiry_and_max_uses() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let missing_expiry = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({ "max_uses": 1 }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(missing_expiry.status(), StatusCode::BAD_REQUEST);

    let missing_max_uses = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({ "expires_at": unix_now() + 86_400 }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(missing_max_uses.status(), StatusCode::BAD_REQUEST);

    configure_required_model_configs(&app, &admin_cookie).await;
    let created = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;

    assert!(created["token"].as_str().expect("token exists").len() >= 32);
    assert_eq!(created["invite"]["max_uses"], 1);
    assert_eq!(created["invite"]["used_count"], 0);
}

#[tokio::test]
async fn invite_creation_requires_ready_llm_and_title_model_configs() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let not_ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(not_ready.status(), StatusCode::CONFLICT);

    let llm_only = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "llm",
            "provider_name": "openai-compatible",
            "provider_base_url": "https://ready-provider.example/v1",
            "provider_api_key": "ready-provider-key",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(llm_only.status(), StatusCode::NO_CONTENT);

    let still_not_ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(still_not_ready.status(), StatusCode::CONFLICT);

    let title = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "title",
            "provider_name": "openai-compatible",
            "provider_base_url": "https://ready-provider.example/v1",
            "provider_api_key": "ready-provider-key",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(title.status(), StatusCode::NO_CONTENT);

    let ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(ready.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn invite_redemption_obeys_expiry_and_max_uses() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 2).await;
    let token = invite["token"].as_str().expect("token exists");

    let first = redeem_invite(&app, token, "first@example.com").await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = redeem_invite(&app, token, "second@example.com").await;
    assert_eq!(second.status(), StatusCode::CREATED);

    let third = redeem_invite(&app, token, "third@example.com").await;
    assert_eq!(third.status(), StatusCode::CONFLICT);

    let expired = create_invite(&app, &admin_cookie, unix_now() - 1, 1).await;
    let expired_token = expired["token"].as_str().expect("token exists");
    let response = redeem_invite(&app, expired_token, "late@example.com").await;

    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test]
async fn revoked_invites_cannot_be_redeemed() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;
    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;
    let invite_id = invite["invite"]["id"].as_str().expect("invite id exists");
    let token = invite["token"].as_str().expect("token exists");

    let response = request_empty(
        &app,
        Method::POST,
        &format!("/api/invites/{invite_id}/revoke"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = redeem_invite(&app, token, "revoked@example.com").await;
    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test]
async fn invite_registration_provisions_a_managed_hermes_instance() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;
    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;
    let token = invite["token"].as_str().expect("token exists");

    let response = redeem_invite(&app, token, "test@example.com").await;
    let (status, registered) = response_json(response).await;
    assert_eq!(status, StatusCode::CREATED);
    let user_id = registered["user"]["id"]
        .as_str()
        .expect("registered user id");

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], user_id);
    assert_eq!(body["hermes_instances"][0]["kind"], "managed_docker");
    assert_eq!(body["hermes_instances"][0]["status"], "running");
}
