use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use crate::{
    hermes::{
        docker_provisioner::{DockerProvisioner, DockerProvisionerConfig},
        instance::HermesInstance,
        provisioner::HermesProvisioner,
    },
    http::{auth::current_user, ApiError},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/workspace/status", get(status))
        .route("/api/workspace/ensure-hermes", post(ensure_hermes))
        .route(
            "/api/workspace/hermes-instance",
            get(current_hermes_instance),
        )
}

#[derive(Serialize)]
struct WorkspaceStatusResponse {
    hermes_instance: Option<HermesInstance>,
}

#[derive(Serialize)]
struct HermesInstanceResponse {
    hermes_instance: HermesInstance,
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers)?;
    let hermes_instance = state.store.hermes_instance_for_user(&user.id).ok();

    Ok(Json(WorkspaceStatusResponse { hermes_instance }))
}

async fn ensure_hermes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers)?;
    if let Ok(instance) = state.store.hermes_instance_for_user(&user.id) {
        return Ok(Json(HermesInstanceResponse {
            hermes_instance: instance,
        }));
    }

    let provisioner = DockerProvisioner::new(DockerProvisionerConfig {
        image: "nousresearch/hermes-agent:latest".to_string(),
        data_root: std::path::PathBuf::from("/tmp/hermes-hub/users"),
        network: "hermes-hub-net".to_string(),
        internal_port: 8000,
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        default_model: state
            .model_registry
            .active_config()
            .map_err(|_| ApiError::Internal)?
            .default_model,
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
    });
    let instance = provisioner
        .ensure_instance(&user.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .store
        .bind_hermes_instance(instance.clone())
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(HermesInstanceResponse {
        hermes_instance: instance,
    }))
}

async fn current_hermes_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers)?;
    let hermes_instance = state
        .store
        .hermes_instance_for_user(&user.id)
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;

    Ok(Json(HermesInstanceResponse { hermes_instance }))
}
