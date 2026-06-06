use hermes_hub_backend::channel::service::{ChannelSessionKind, ChannelStore, ChannelStoreError};

#[tokio::test]
async fn home_session_is_pinned_protected_and_excluded_from_session_limit() {
    let store = ChannelStore::default();
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
    let store = ChannelStore::default();

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
