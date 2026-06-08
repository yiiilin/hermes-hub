use hermes_hub_backend::channel::service::{ChannelSessionKind, ChannelStore, ChannelStoreError};

#[tokio::test]
async fn home_session_is_pinned_protected_and_excluded_from_session_limit() {
    let store = ChannelStore::in_memory_for_tests();
    let channel = store
        .ensure_hub_channel("user-1")
        .await
        .expect("hub channel can be created");

    let home = store
        .ensure_home_session("user-1", &channel.id)
        .await
        .expect("home session can be created");
    assert!(home.is_home);
    assert!(!home.deletable);

    let duplicate = store
        .ensure_home_session("user-1", &channel.id)
        .await
        .expect("home session creation is idempotent");
    assert_eq!(duplicate.id, home.id);

    let regular = store
        .create_session_with_limit(
            "user-1",
            &channel.id,
            ChannelSessionKind::Agent,
            None,
            1,
            false,
        )
        .await
        .expect("home session does not consume the regular session quota");
    assert!(!regular.is_home);
    assert!(regular.deletable);

    let sessions = store
        .list_sessions("user-1", &channel.id)
        .await
        .expect("sessions can be listed");
    assert_eq!(
        sessions.first().map(|session| session.id.as_str()),
        Some(home.id.as_str())
    );

    let second_regular = store
        .create_session_with_limit(
            "user-1",
            &channel.id,
            ChannelSessionKind::Agent,
            None,
            1,
            false,
        )
        .await;
    assert!(matches!(
        second_regular,
        Err(ChannelStoreError::SessionLimitExceeded {
            max_sessions_per_user: 1
        })
    ));

    let deleted_home = store.delete_session("user-1", &channel.id, &home.id).await;
    assert!(matches!(
        deleted_home,
        Err(ChannelStoreError::ProtectedSession)
    ));
}

#[tokio::test]
async fn integration_channel_is_stable_per_user_and_integration() {
    let store = ChannelStore::in_memory_for_tests();

    let crm_for_user_1 = store
        .ensure_integration_channel("user-1", "crm-client")
        .await
        .expect("integration channel can be created");
    let crm_for_user_1_again = store
        .ensure_integration_channel("user-1", " crm-client ")
        .await
        .expect("integration channel can be reused");
    let erp_for_user_1 = store
        .ensure_integration_channel("user-1", "erp-client")
        .await
        .expect("second integration channel can be created");
    let crm_for_user_2 = store
        .ensure_integration_channel("user-2", "crm-client")
        .await
        .expect("same integration can be created for another user");

    assert_eq!(crm_for_user_1.id, crm_for_user_1_again.id);
    assert_eq!(crm_for_user_1.name, "integration:crm-client");
    assert_ne!(crm_for_user_1.id, erp_for_user_1.id);
    assert_ne!(crm_for_user_1.id, crm_for_user_2.id);
    assert!(matches!(
        store.ensure_integration_channel("user-1", " ").await,
        Err(ChannelStoreError::InvalidIntegrationId)
    ));
}

#[tokio::test]
async fn hidden_sessions_do_not_consume_visible_session_limit() {
    let store = ChannelStore::in_memory_for_tests();
    let channel = store
        .ensure_hub_channel("user-1")
        .await
        .expect("hub channel can be created");

    // hidden_from_web 目前用于 integration session；这里直接在 store 层锁住配额语义，
    // 避免后续改动把隐藏会话重新算进 Web 可见会话上限。
    let hidden = store
        .create_session("user-1", &channel.id, ChannelSessionKind::Agent, None, true)
        .await
        .expect("hidden session can be created");
    assert!(hidden.hidden_from_web);

    let visible = store
        .create_session_with_limit(
            "user-1",
            &channel.id,
            ChannelSessionKind::Agent,
            None,
            1,
            false,
        )
        .await
        .expect("hidden session must not consume visible session quota");
    assert!(!visible.hidden_from_web);

    let second_visible = store
        .create_session_with_limit(
            "user-1",
            &channel.id,
            ChannelSessionKind::Agent,
            None,
            1,
            false,
        )
        .await;
    assert!(matches!(
        second_visible,
        Err(ChannelStoreError::SessionLimitExceeded {
            max_sessions_per_user: 1
        })
    ));
}

#[tokio::test]
async fn memory_run_lookup_rejects_unbound_channel_for_specific_instance() {
    let store = ChannelStore::in_memory_for_tests();
    let channel = store
        .ensure_hub_channel("user-1")
        .await
        .expect("hub channel can be created");
    let session = store
        .create_session(
            "user-1",
            &channel.id,
            ChannelSessionKind::Agent,
            None,
            false,
        )
        .await
        .expect("session can be created");
    let message = store
        .append_session_message(
            "user-1",
            &channel.id,
            &session.id,
            hermes_hub_backend::channel::service::ChannelMessageRole::User,
            Some("unbound-run-message".to_string()),
            "hello".to_string(),
            serde_json::json!([]),
        )
        .await
        .expect("message can be created");
    let run = store
        .create_channel_run(
            "user-1",
            &channel.id,
            &session.id,
            &message.id,
            "hello".to_string(),
            serde_json::json!([]),
        )
        .await
        .expect("run can be created");

    let looked_up = store
        .get_run_for_instance(Some("instance-1"), &run.run_id)
        .await;
    assert!(matches!(looked_up, Err(ChannelStoreError::RunNotFound)));
}
