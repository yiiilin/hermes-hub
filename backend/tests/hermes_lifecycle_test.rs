use hermes_hub_backend::hermes::{
    instance::HermesInstanceStatus,
    lifecycle::{
        decide_hermes_lifecycle_action, HermesLifecycleAction, HermesLifecycleDecisionInput,
        HermesLifecycleDecisionSettings, HermesLifecycleScheduledTask,
    },
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
