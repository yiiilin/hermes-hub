use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{build_router, AppConfig};
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use tower::ServiceExt;
use zip::{write::SimpleFileOptions, ZipWriter};

fn test_app() -> Router {
    build_router(AppConfig::for_tests())
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
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
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

fn multipart_text(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"{name}\"\r\n\r\n\
             {value}\r\n"
        )
        .as_bytes(),
    );
}

fn multipart_file(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) {
    body.extend_from_slice(
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
             Content-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
}

fn finish_multipart(body: &mut Vec<u8>, boundary: &str) {
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (path, bytes) in entries {
        writer.start_file(path, options).expect("zip file entry");
        writer.write_all(bytes).expect("zip entry bytes");
    }
    writer
        .finish()
        .expect("zip archive can be finished")
        .into_inner()
}

fn tree_child<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["children"]
        .as_array()
        .expect("tree node has children")
        .iter()
        .find(|child| child["name"] == name)
        .unwrap_or_else(|| panic!("missing tree child {name}"))
}

async fn bootstrap_admin(app: &Router) -> String {
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

#[tokio::test]
async fn admin_workspace_test() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (status, body) = response_json(users).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"][0]["email"], "admin@example.com");
    let admin_id = body["users"][0]["id"].as_str().expect("admin id");

    let disable = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/disable"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(disable.status(), StatusCode::CONFLICT);

    let update_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "responses",
            "reasoning_effort": "medium",
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);

    let model = request_empty(
        &app,
        Method::GET,
        "/api/admin/model-config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["model_config"]["provider_name"], "custom");
    assert_eq!(body["model_config"]["default_model"], "gpt-4.1");
    assert_eq!(body["model_config"]["provider_api_key"], "secret-v2");
    assert_eq!(body["model_config"]["api_type"], "responses");
    assert_eq!(body["model_config"]["reasoning_effort"], "medium");

    let status_response = request_empty(
        &app,
        Method::GET,
        "/api/workspace/status",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(status_response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["hermes_instance"].is_null());

    let blocked_without_title_model = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(blocked_without_title_model.status(), StatusCode::CONFLICT);

    let blocked_admin_create_without_title_model = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        blocked_admin_create_without_title_model.status(),
        StatusCode::CONFLICT
    );

    let update_title_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "title",
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "title-secret-v2",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_title_model.status(), StatusCode::NO_CONTENT);

    let update_image_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "image",
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "image-secret-v2",
            "default_model": "gpt-image-1",
            "allowed_models": ["gpt-image-1"],
            "allow_streaming": false,
            "request_timeout_seconds": 180
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_image_model.status(), StatusCode::NO_CONTENT);

    let created_by_admin = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(created_by_admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");
    let managed_config = std::fs::read_to_string(format!(
        "/tmp/hermes-hub/users/{admin_id}/config/config.yaml"
    ))
    .expect("managed Hermes config is written");
    assert!(managed_config.contains("model: \"gpt-image-1\""));
    assert!(!managed_config.contains("gpt-image-2-medium"));

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");

    let stop = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/stop"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(stop).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "stopped");

    let start = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/start"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(start).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "running");

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], admin_id);

    let removed_legacy_config = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/users/{admin_id}/hermes-instance/external-config"),
        json!({
            "name": "legacy runtime",
            "api_token": "external-token"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(removed_legacy_config.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_configure_per_user_session_limit() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 20);
    assert_eq!(body["settings"]["oidc"]["enabled"], false);
    assert_eq!(body["settings"]["oidc"]["display_name"], "OpenID Connect");

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 2,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "issuer_url": "https://idp.example.com",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "logout_url": "https://idp.example.com/logout",
                "scopes": "openid profile email",
                "username_claim": "preferred_username",
                "email_claim": "email",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 2);
    assert_eq!(body["settings"]["oidc"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["display_name"], "Acme SSO");
    assert_eq!(body["settings"]["oidc"]["client_id"], "hermes-hub");
    assert_eq!(body["settings"]["oidc"]["client_secret"], "oidc-secret");
    assert_eq!(
        body["settings"]["oidc"]["authorization_url"],
        "https://idp.example.com/oauth2/v1/authorize"
    );

    let public_oidc = request_empty(&app, Method::GET, "/api/auth/oidc/config", None).await;
    let (status, body) = response_json(public_oidc).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["oidc"]["enabled"], true);
    assert_eq!(body["oidc"]["display_name"], "Acme SSO");
    assert!(body["oidc"].get("client_secret").is_none());

    let channels = request_empty(&app, Method::GET, "/api/channels", Some(&admin_cookie)).await;
    let (status, body) = response_json(channels).await;
    assert_eq!(status, StatusCode::OK);
    let channel_id = body["channels"][0]
        .as_object()
        .and_then(|channel| channel.get("id"))
        .and_then(Value::as_str)
        .expect("channel id");

    for _ in 0..2 {
        let created = request_json(
            &app,
            Method::POST,
            &format!("/api/channels/{channel_id}/sessions"),
            json!({ "kind": "agent" }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let blocked = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(blocked).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "session_limit_exceeded");
    assert_eq!(body["message"], "session limit exceeded");
    assert_eq!(body["max_sessions_per_user"], 2);
}

#[tokio::test]
async fn admin_can_manage_hub_skills() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let save = request_json(
        &app,
        Method::PUT,
        "/api/admin/managed-skills/writing/SKILL.md",
        json!({
            "content": "# Writing\n\nUse concise prose.\n"
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(save).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skill"]["path"], "writing/SKILL.md");
    assert_eq!(
        body["skill"]["content"],
        "# Writing\n\nUse concise prose.\n"
    );

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skills"][0]["path"], "writing/SKILL.md");
    assert_eq!(body["skills"][0]["size"], 30);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(read).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["skill"]["content"],
        "# Writing\n\nUse concise prose.\n"
    );

    let hidden = request_json(
        &app,
        Method::PUT,
        "/api/admin/managed-skills/.curator_state/state.json",
        json!({ "content": "{}" }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);

    let delete = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let read_deleted = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(read_deleted.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_view_managed_skills_as_a_file_tree() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    for (path, content) in [
        ("writing/SKILL.md", "# Writing\n"),
        ("writing/references/style.md", "Be precise.\n"),
        ("image/SKILL.md", "# Image\n"),
    ] {
        let save = request_json(
            &app,
            Method::PUT,
            &format!("/api/admin/managed-skills/{path}"),
            json!({ "content": content }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK);
    }

    let tree = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/tree",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(tree).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tree"]["kind"], "dir");
    assert_eq!(body["tree"]["path"], "");

    let image = tree_child(&body["tree"], "image");
    assert_eq!(image["kind"], "dir");
    assert_eq!(tree_child(image, "SKILL.md")["kind"], "file");

    let writing = tree_child(&body["tree"], "writing");
    assert_eq!(writing["kind"], "dir");
    assert_eq!(tree_child(writing, "SKILL.md")["size"], 10);
    let references = tree_child(writing, "references");
    assert_eq!(
        tree_child(references, "style.md")["path"],
        "writing/references/style.md"
    );
}

#[tokio::test]
async fn admin_can_create_empty_managed_skill_directories() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let create = request_empty(
        &app,
        Method::POST,
        "/api/admin/managed-skills/directories/research/references",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(create.status(), StatusCode::CREATED);

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["skills"].as_array().expect("skills").is_empty());

    let tree = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/tree",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(tree).await;
    assert_eq!(status, StatusCode::OK);
    let research = tree_child(&body["tree"], "research");
    assert_eq!(research["kind"], "dir");
    assert_eq!(tree_child(research, "references")["kind"], "dir");

    let hidden = request_empty(
        &app,
        Method::POST,
        "/api/admin/managed-skills/directories/.curator_state",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_can_delete_managed_skill_directories_recursively() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    for path in [
        "writing/SKILL.md",
        "writing/references/style.md",
        "image/SKILL.md",
    ] {
        let save = request_json(
            &app,
            Method::PUT,
            &format!("/api/admin/managed-skills/{path}"),
            json!({ "content": path }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK);
    }

    let delete = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    let paths = body["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|skill| skill["path"].as_str().expect("skill path"))
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["image/SKILL.md"]);

    let delete_missing = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete_missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_upload_managed_skill_files_and_folders() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-upload-boundary";
    let mut upload_body = Vec::new();
    multipart_text(&mut upload_body, boundary, "target_path", "packs");
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "research/SKILL.md",
        "text/markdown",
        b"# Research\n",
    );
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "research/references/paper.md",
        "text/markdown",
        b"Read primary sources.\n",
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["skills"].as_array().expect("uploaded skills").len(), 2);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/packs/research/references/paper.md",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(read).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skill"]["content"], "Read primary sources.\n");
}

#[tokio::test]
async fn admin_can_upload_managed_skill_zip_archives() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-zip-boundary";
    let archive = zip_bytes(&[
        ("assistant/SKILL.md", b"# Assistant\n" as &[u8]),
        ("assistant/references/tone.md", b"Be direct.\n" as &[u8]),
    ]);
    let mut upload_body = Vec::new();
    multipart_text(&mut upload_body, boundary, "target_path", "bundles");
    multipart_file(
        &mut upload_body,
        boundary,
        "file",
        "skills.zip",
        "application/zip",
        &archive,
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["skills"].as_array().expect("uploaded skills").len(), 2);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/bundles/assistant/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(read).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skill"]["content"], "# Assistant\n");
}

#[tokio::test]
async fn managed_skill_upload_rejects_unsafe_paths() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let boundary = "managed-skills-unsafe-folder-boundary";
    let mut unsafe_folder_body = Vec::new();
    multipart_file(
        &mut unsafe_folder_body,
        boundary,
        "files",
        "../SKILL.md",
        "text/markdown",
        b"escaped",
    );
    finish_multipart(&mut unsafe_folder_body, boundary);
    let unsafe_folder = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        unsafe_folder_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(unsafe_folder.status(), StatusCode::BAD_REQUEST);

    let boundary = "managed-skills-hidden-upload-boundary";
    let mut hidden_body = Vec::new();
    multipart_file(
        &mut hidden_body,
        boundary,
        "files",
        ".curator_state/state.json",
        "application/json",
        b"{}",
    );
    finish_multipart(&mut hidden_body, boundary);
    let hidden = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        hidden_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);

    let boundary = "managed-skills-unsafe-zip-boundary";
    let archive = zip_bytes(&[("../escape.md", b"escaped" as &[u8])]);
    let mut unsafe_zip_body = Vec::new();
    multipart_file(
        &mut unsafe_zip_body,
        boundary,
        "file",
        "skills.zip",
        "application/zip",
        &archive,
    );
    finish_multipart(&mut unsafe_zip_body, boundary);
    let unsafe_zip = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        unsafe_zip_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(unsafe_zip.status(), StatusCode::BAD_REQUEST);
}
