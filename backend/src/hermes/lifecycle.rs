use std::time::Duration;

use crate::{
    hermes::{
        instance::{HermesInstance, HermesInstanceStatus},
        provisioner::HermesProvisioner,
    },
    http::{map_provisioner_error, workspace::ensure_managed_hermes_for_user_without_activity},
    public_platform,
    session::store::{
        HermesLifecycleCandidate, HermesScheduledTaskSnapshot, HermesSchedulerSnapshot,
    },
    AppState,
};

pub const DEFAULT_IDLE_STOP_AFTER_SECONDS: u64 = 30 * 60;
pub const DEFAULT_WAKE_MARGIN_SECONDS: u64 = 5 * 60;
pub const DEFAULT_SWEEP_INTERVAL_SECONDS: u64 = 60;
const IDLE_STOP_REASON: &str = "idle";
const PUBLIC_PLATFORM_DISABLED_STOP_REASON: &str = "public_platform_disabled";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesLifecycleDecisionSettings {
    pub idle_stop_after_seconds: u64,
    pub wake_margin_seconds: u64,
}

impl Default for HermesLifecycleDecisionSettings {
    fn default() -> Self {
        Self {
            idle_stop_after_seconds: DEFAULT_IDLE_STOP_AFTER_SECONDS,
            wake_margin_seconds: DEFAULT_WAKE_MARGIN_SECONDS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesLifecycleScheduledTask {
    pub enabled: bool,
    pub next_run_at: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesLifecycleDecisionInput {
    pub status: HermesInstanceStatus,
    pub last_user_activity_at: Option<u64>,
    pub has_active_runs: bool,
    pub scheduler_enabled: bool,
    pub tasks: Vec<HermesLifecycleScheduledTask>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HermesLifecycleAction {
    KeepRunning,
    StopIdle,
    WakeForScheduledTask,
}

pub fn decide_hermes_lifecycle_action(
    input: &HermesLifecycleDecisionInput,
    now: u64,
    settings: HermesLifecycleDecisionSettings,
) -> HermesLifecycleAction {
    if input.has_active_runs {
        return HermesLifecycleAction::KeepRunning;
    }

    if matches!(input.status, HermesInstanceStatus::Stopped)
        && input.scheduler_enabled
        && has_task_requiring_runtime(&input.tasks, now, settings.wake_margin_seconds)
    {
        return HermesLifecycleAction::WakeForScheduledTask;
    }

    if !matches!(input.status, HermesInstanceStatus::Running) {
        return HermesLifecycleAction::KeepRunning;
    }

    if input.scheduler_enabled
        && has_task_requiring_runtime(&input.tasks, now, settings.wake_margin_seconds)
    {
        return HermesLifecycleAction::KeepRunning;
    }

    let Some(last_user_activity_at) = input.last_user_activity_at else {
        return HermesLifecycleAction::KeepRunning;
    };
    if now.saturating_sub(last_user_activity_at) >= settings.idle_stop_after_seconds {
        HermesLifecycleAction::StopIdle
    } else {
        HermesLifecycleAction::KeepRunning
    }
}

pub async fn start_hermes_lifecycle_sweeper(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(DEFAULT_SWEEP_INTERVAL_SECONDS));
    loop {
        interval.tick().await;
        if let Err(error) = crate::http::sessions::cleanup_expired_public_sessions(&state).await {
            tracing::warn!(?error, "Public platform session cleanup failed");
        }
        if let Err(error) = sweep_hermes_lifecycle_once(&state, unix_now()).await {
            tracing::warn!(?error, "Hermes lifecycle sweep failed");
        }
    }
}

pub async fn sweep_hermes_lifecycle_once(
    state: &AppState,
    now: u64,
) -> Result<(), crate::http::ApiError> {
    if let Err(error) = public_platform::ensure_public_hermes_if_enabled(state).await {
        tracing::warn!(?error, "failed to keep public platform Hermes running");
    }
    let public_user_id = public_platform::public_user_id(state).await?;
    let public_platform_enabled = public_platform::configured_enabled(state).await?;
    let settings = HermesLifecycleDecisionSettings::default();
    let candidates = state
        .store
        .list_hermes_lifecycle_candidates()
        .await
        .map_err(|_| crate::http::ApiError::Internal)?;

    for candidate in candidates {
        if public_user_id.as_deref() == Some(candidate.instance.user_id.as_str()) {
            if public_platform_enabled {
                if !public_platform::is_public_hermes_instance_ready(&candidate.instance) {
                    if let Err(error) = ensure_managed_hermes_for_user_without_activity(
                        state,
                        &candidate.instance.user_id,
                    )
                    .await
                    {
                        tracing::warn!(
                            ?error,
                            instance_id = %candidate.instance.id,
                            "failed to restart public platform Hermes instance"
                        );
                    }
                }
            } else if matches!(
                candidate.instance.status,
                HermesInstanceStatus::Running | HermesInstanceStatus::Provisioning
            ) {
                if let Err(error) = stop_instance_with_reason(
                    state,
                    &candidate.instance,
                    PUBLIC_PLATFORM_DISABLED_STOP_REASON,
                )
                .await
                {
                    tracing::warn!(
                        ?error,
                        instance_id = %candidate.instance.id,
                        "failed to stop disabled public platform Hermes instance"
                    );
                }
            }
            // 公共 Hermes 不参与普通用户 idle stop / scheduler wake；关闭时也不能被调度重新唤醒。
            continue;
        }
        let has_active_runs = state
            .channel_store
            .instance_has_active_runs(&candidate.instance.id)
            .await
            .map_err(|_| crate::http::ApiError::Internal)?;
        let decision = decide_hermes_lifecycle_action(
            &decision_input(&candidate, has_active_runs),
            now,
            settings.clone(),
        );
        match decision {
            HermesLifecycleAction::KeepRunning => {}
            HermesLifecycleAction::StopIdle => {
                if let Err(error) = stop_idle_instance(state, &candidate.instance).await {
                    tracing::warn!(
                        ?error,
                        instance_id = %candidate.instance.id,
                        "failed to stop idle Hermes instance"
                    );
                }
            }
            HermesLifecycleAction::WakeForScheduledTask => {
                if let Err(error) = ensure_managed_hermes_for_user_without_activity(
                    state,
                    &candidate.instance.user_id,
                )
                .await
                {
                    tracing::warn!(
                        ?error,
                        instance_id = %candidate.instance.id,
                        "failed to wake Hermes instance for scheduled task"
                    );
                }
            }
        }
    }

    Ok(())
}

fn decision_input(
    candidate: &HermesLifecycleCandidate,
    has_active_runs: bool,
) -> HermesLifecycleDecisionInput {
    let snapshot = candidate.scheduler_snapshot.as_ref();
    HermesLifecycleDecisionInput {
        status: candidate.instance.status.clone(),
        last_user_activity_at: candidate.lifecycle.last_user_activity_at,
        has_active_runs,
        scheduler_enabled: snapshot
            .map(|snapshot| snapshot.scheduler_enabled && snapshot.scheduler_status == "ok")
            .unwrap_or(false),
        tasks: snapshot_tasks(snapshot),
    }
}

fn snapshot_tasks(snapshot: Option<&HermesSchedulerSnapshot>) -> Vec<HermesLifecycleScheduledTask> {
    snapshot
        .map(|snapshot| {
            snapshot
                .tasks
                .iter()
                .map(task_from_snapshot)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn task_from_snapshot(task: &HermesScheduledTaskSnapshot) -> HermesLifecycleScheduledTask {
    HermesLifecycleScheduledTask {
        enabled: task.enabled,
        next_run_at: task.next_run_at,
    }
}

fn has_task_requiring_runtime(
    tasks: &[HermesLifecycleScheduledTask],
    now: u64,
    margin_seconds: u64,
) -> bool {
    tasks.iter().any(|task| {
        task.enabled
            // Hermes 的 interval 任务当前可能无法上报精确 next_run_at；
            // 对这类启用任务必须保守保活，否则会直接错过后续执行。
            && task.next_run_at.map_or(true, |next_run_at| {
                next_run_at <= now.saturating_add(margin_seconds)
            })
    })
}

async fn stop_idle_instance(
    state: &AppState,
    instance: &HermesInstance,
) -> Result<(), crate::http::ApiError> {
    stop_instance_with_reason(state, instance, IDLE_STOP_REASON).await
}

async fn stop_instance_with_reason(
    state: &AppState,
    instance: &HermesInstance,
    reason: &str,
) -> Result<(), crate::http::ApiError> {
    let stopped = state
        .docker_provisioner
        .stop_instance(instance)
        .await
        .map_err(map_provisioner_error)?;
    state
        .store
        .bind_hermes_instance(stopped.clone())
        .await
        .map_err(|_| crate::http::ApiError::Internal)?;
    state
        .store
        .set_hermes_instance_stopped_reason(&stopped.id, reason)
        .await
        .map_err(|_| crate::http::ApiError::Internal)?;
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}
