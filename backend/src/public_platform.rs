use crate::{
    hermes::instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    http::{workspace, ApiError},
    AppState,
};

#[derive(Clone, Debug)]
pub struct PublicHermesReadiness {
    pub configured_enabled: bool,
    pub ready: bool,
    pub hermes_instance: Option<HermesInstance>,
}

pub async fn configured_enabled(state: &AppState) -> Result<bool, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    // 公共平台只由管理员系统设置控制，避免部署变量和运行时配置互相覆盖。
    Ok(settings.public_platform.enabled)
}

pub async fn public_user_id(state: &AppState) -> Result<Option<String>, ApiError> {
    state
        .store
        .public_platform_user_id()
        .await
        .map_err(|_| ApiError::Internal)
}

pub async fn is_public_user_id(state: &AppState, user_id: &str) -> Result<bool, ApiError> {
    Ok(public_user_id(state).await?.as_deref() == Some(user_id))
}

pub async fn ensure_public_owner_user_id(state: &AppState) -> Result<String, ApiError> {
    let has_active_admin = state
        .store
        .first_active_admin_user_id()
        .await
        .map_err(|_| ApiError::Internal)?
        .is_some();
    if !has_active_admin {
        return Err(ApiError::Unauthorized);
    }

    // 公共平台所有匿名会话都挂在隐藏系统用户上，避免污染真实管理员的 workspace。
    let public_user = state
        .store
        .ensure_public_platform_user()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(public_user.id)
}

pub async fn ensure_public_hermes_if_enabled(
    state: &AppState,
) -> Result<Option<HermesInstance>, ApiError> {
    if !configured_enabled(state).await? {
        return Ok(None);
    }
    let public_user_id = ensure_public_owner_user_id(state).await?;
    let instance =
        workspace::ensure_managed_hermes_for_user_without_activity(state, &public_user_id).await?;
    Ok(Some(instance))
}

pub async fn public_hermes_readiness(state: &AppState) -> Result<PublicHermesReadiness, ApiError> {
    let configured_enabled = configured_enabled(state).await?;
    if !configured_enabled {
        return Ok(PublicHermesReadiness {
            configured_enabled,
            ready: false,
            hermes_instance: None,
        });
    }

    let Some(public_user_id) = public_user_id(state).await? else {
        return Ok(PublicHermesReadiness {
            configured_enabled,
            ready: false,
            hermes_instance: None,
        });
    };
    let instance = match state.store.hermes_instance_for_user(&public_user_id).await {
        Ok(instance) => Some(workspace::refresh_managed_hermes_status(state, instance).await?),
        Err(_) => None,
    };
    let ready = instance
        .as_ref()
        .is_some_and(is_public_hermes_instance_ready);

    Ok(PublicHermesReadiness {
        configured_enabled,
        ready,
        hermes_instance: instance,
    })
}

pub fn is_public_hermes_instance_ready(instance: &HermesInstance) -> bool {
    if instance.kind != HermesInstanceKind::ManagedDocker {
        return false;
    }
    if !matches!(instance.status, HermesInstanceStatus::Running) {
        return false;
    }
    if instance.host_sandbox_path.as_deref().is_none() {
        return false;
    }

    let health = instance.health_status.trim();
    !matches!(health, "missing" | "unhealthy" | "stopped" | "error")
}
