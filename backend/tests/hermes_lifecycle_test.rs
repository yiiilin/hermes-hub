use hermes_hub_backend::hermes::{
    docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    instance::HermesInstanceStatus,
    lifecycle::{
        decide_hermes_lifecycle_action, sweep_hermes_lifecycle_once, HermesLifecycleAction,
        HermesLifecycleDecisionInput, HermesLifecycleDecisionSettings,
        HermesLifecycleScheduledTask,
    },
};
use hermes_hub_backend::{
    asr,
    channel::{events::SessionEventHub, service::ChannelStore},
    docker_config_from_app,
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::ModelRegistry,
    session::store::{HermesScheduledTaskSnapshot, HermesSchedulerSnapshotInput, SessionStore},
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

fn settings() -> HermesLifecycleDecisionSettings {
    HermesLifecycleDecisionSettings {
        idle_stop_after_seconds: 30 * 60,
        wake_margin_seconds: 5 * 60,
    }
}

fn task(next_run_at: u64) -> HermesLifecycleScheduledTask {
    HermesLifecycleScheduledTask {
        enabled: true,
        next_run_at: Some(next_run_at),
    }
}

fn enabled_task_without_known_next_run() -> HermesLifecycleScheduledTask {
    HermesLifecycleScheduledTask {
        enabled: true,
        next_run_at: None,
    }
}

fn disabled_task_without_known_next_run() -> HermesLifecycleScheduledTask {
    HermesLifecycleScheduledTask {
        enabled: false,
        next_run_at: None,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

fn ready_test_state(store: SessionStore) -> AppState {
    let mut config = AppConfig::for_tests();
    config.initial_model_config.provider_base_url = "https://ready-provider.example/v1".into();
    config.initial_model_config.provider_api_key = "ready-provider-key".into();
    let asr_client = asr::default_asr_client(&config.speech_input);
    let model_registry = ModelRegistry::new(config.initial_model_config.clone());
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store,
        channel_store: ChannelStore::default(),
        model_registry,
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: SessionEventHub::default(),
        asr_client,
    }
}

#[tokio::test]
async fn lifecycle_sweep_keeps_public_platform_hermes_running_and_stops_idle_user_instances() {
    let store = SessionStore::default();
    let admin = store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("admin can be created");
    let public_user = store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");
    let mut settings = store.system_settings().await.expect("settings can be read");
    settings.public_platform.enabled = true;
    store
        .update_system_settings(settings)
        .await
        .expect("public platform can be enabled");
    let state = ready_test_state(store.clone());

    let public_instance = state
        .docker_provisioner
        .prepare_instance_with_sandbox(&public_user.id, true);
    store
        .bind_hermes_instance(public_instance)
        .await
        .expect("public Hermes instance can be bound");
    let user_instance = state.docker_provisioner.prepare_instance(&admin.id);
    store
        .bind_hermes_instance(user_instance)
        .await
        .expect("ordinary user Hermes instance can be bound");

    sweep_hermes_lifecycle_once(&state, unix_now() + 60 * 60)
        .await
        .expect("lifecycle sweep succeeds");

    let public_after = store
        .hermes_instance_for_user(&public_user.id)
        .await
        .expect("public Hermes instance exists");
    assert_eq!(public_after.status, HermesInstanceStatus::Running);
    assert!(public_after.host_sandbox_path.is_some());

    let user_after = store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("ordinary Hermes instance exists");
    assert_eq!(user_after.status, HermesInstanceStatus::Stopped);
}

#[tokio::test]
async fn lifecycle_sweep_does_not_wake_disabled_public_platform_hermes_for_scheduled_tasks() {
    let store = SessionStore::default();
    store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("admin can be created");
    let public_user = store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");
    let state = ready_test_state(store.clone());
    let mut public_instance = state
        .docker_provisioner
        .prepare_instance_with_sandbox(&public_user.id, true);
    public_instance.status = HermesInstanceStatus::Stopped;
    store
        .bind_hermes_instance(public_instance.clone())
        .await
        .expect("public Hermes instance can be bound");
    store
        .record_hermes_scheduler_snapshot(
            &public_instance.id,
            HermesSchedulerSnapshotInput {
                scheduler_status: "ok".to_string(),
                scheduler_enabled: true,
                running_jobs_count: 0,
                reported_at: unix_now(),
                source: "public-scheduler".to_string(),
                snapshot_hash: Some("public-disabled".to_string()),
                next_wake_at: Some(unix_now() + 60),
                tasks: vec![HermesScheduledTaskSnapshot {
                    id: "public-task".to_string(),
                    name: "Public disabled task".to_string(),
                    enabled: true,
                    schedule: "* * * * *".to_string(),
                    timezone: "UTC".to_string(),
                    next_run_at: Some(unix_now() + 60),
                    last_run_at: None,
                    status: "scheduled".to_string(),
                    source: "hermes-adapter".to_string(),
                }],
            },
        )
        .await
        .expect("public scheduler snapshot can be stored");

    sweep_hermes_lifecycle_once(&state, unix_now())
        .await
        .expect("lifecycle sweep succeeds");

    let public_after = store
        .hermes_instance_for_user(&public_user.id)
        .await
        .expect("public Hermes instance exists");
    assert_eq!(public_after.status, HermesInstanceStatus::Stopped);
}

#[tokio::test]
async fn lifecycle_sweep_stops_running_public_platform_hermes_when_disabled() {
    let store = SessionStore::default();
    store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("admin can be created");
    let public_user = store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");
    let state = ready_test_state(store.clone());
    let public_instance = state
        .docker_provisioner
        .prepare_instance_with_sandbox(&public_user.id, true);
    store
        .bind_hermes_instance(public_instance)
        .await
        .expect("public Hermes instance can be bound");

    sweep_hermes_lifecycle_once(&state, unix_now())
        .await
        .expect("lifecycle sweep succeeds");

    let public_after = store
        .hermes_instance_for_user(&public_user.id)
        .await
        .expect("public Hermes instance exists");
    assert_eq!(public_after.status, HermesInstanceStatus::Stopped);
    assert_eq!(
        public_after.stopped_reason.as_deref(),
        Some("public_platform_disabled")
    );
}

#[test]
fn lifecycle_decision_stops_idle_running_instances_when_no_task_is_due() {
    let now = 1_735_689_600;
    let decision = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Running,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![task(now + 60 * 60)],
        },
        now,
        settings(),
    );

    assert_eq!(decision, HermesLifecycleAction::StopIdle);
}

#[test]
fn lifecycle_decision_keeps_running_instances_for_active_or_imminent_work() {
    let now = 1_735_689_600;
    let active_run = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Running,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: true,
            scheduler_enabled: false,
            tasks: Vec::new(),
        },
        now,
        settings(),
    );
    let imminent_task = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Running,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![task(now + 60)],
        },
        now,
        settings(),
    );

    assert_eq!(active_run, HermesLifecycleAction::KeepRunning);
    assert_eq!(imminent_task, HermesLifecycleAction::KeepRunning);
}

#[test]
fn lifecycle_decision_keeps_running_instances_for_enabled_tasks_with_unknown_wake_time() {
    let now = 1_735_689_600;
    let enabled_unknown_task = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Running,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![enabled_task_without_known_next_run()],
        },
        now,
        settings(),
    );
    let disabled_unknown_task = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Running,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![disabled_task_without_known_next_run()],
        },
        now,
        settings(),
    );

    assert_eq!(enabled_unknown_task, HermesLifecycleAction::KeepRunning);
    assert_eq!(disabled_unknown_task, HermesLifecycleAction::StopIdle);
}

#[test]
fn lifecycle_decision_wakes_stopped_instances_before_due_tasks() {
    let now = 1_735_689_600;
    let decision = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Stopped,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![task(now + 60)],
        },
        now,
        settings(),
    );

    assert_eq!(decision, HermesLifecycleAction::WakeForScheduledTask);
}

#[test]
fn lifecycle_decision_wakes_stopped_instances_for_enabled_tasks_with_unknown_wake_time() {
    let now = 1_735_689_600;
    let decision = decide_hermes_lifecycle_action(
        &HermesLifecycleDecisionInput {
            status: HermesInstanceStatus::Stopped,
            last_user_activity_at: Some(now - 60 * 60),
            has_active_runs: false,
            scheduler_enabled: true,
            tasks: vec![enabled_task_without_known_next_run()],
        },
        now,
        settings(),
    );

    assert_eq!(decision, HermesLifecycleAction::WakeForScheduledTask);
}
