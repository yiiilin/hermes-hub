use hermes_hub_backend::session::store::{
    PublicPlatformSettings, SessionStore, StoreError, SystemSettings, PUBLIC_PLATFORM_USER_EMAIL,
};
use uuid::Uuid;

#[tokio::test]
async fn system_settings_include_public_platform_retention_hours() {
    let store = SessionStore::in_memory_for_tests();

    let defaults = store
        .system_settings()
        .await
        .expect("default settings can be read");
    assert_eq!(
        defaults.public_platform.temporary_session_retention_hours,
        24
    );

    store
        .update_system_settings(SystemSettings {
            public_platform: PublicPlatformSettings {
                enabled: false,
                temporary_session_retention_hours: 48,
            },
            ..defaults
        })
        .await
        .expect("public platform settings can be saved");

    let reloaded = store
        .system_settings()
        .await
        .expect("updated settings can be read");
    assert_eq!(
        reloaded.public_platform.temporary_session_retention_hours,
        48
    );
}

#[tokio::test]
async fn hidden_public_platform_user_does_not_close_bootstrap_registration() {
    let store = SessionStore::in_memory_for_tests();
    let public_user = store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");

    assert!(store
        .bootstrap_open()
        .await
        .expect("bootstrap status can be read"));
    let admin = store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("first human admin can still be created");

    assert_ne!(admin.id, public_user.id);
    assert!(!store
        .bootstrap_open()
        .await
        .expect("bootstrap status can be read after admin creation"));
}

#[tokio::test]
async fn public_platform_identity_cannot_be_used_as_a_login_account() {
    let store = SessionStore::in_memory_for_tests();
    let public_user = store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");
    let token = store
        .create_session(&public_user.id)
        .await
        .expect("legacy public user session can be created for regression coverage");

    assert!(matches!(
        store
            .login(PUBLIC_PLATFORM_USER_EMAIL, "any-password")
            .await,
        Err(StoreError::InvalidCredentials)
    ));
    assert!(matches!(
        store
            .get_or_create_oidc_user(PUBLIC_PLATFORM_USER_EMAIL, true)
            .await,
        Err(StoreError::InvalidCredentials)
    ));
    assert!(matches!(
        store
            .get_or_create_ldap_user(PUBLIC_PLATFORM_USER_EMAIL, true)
            .await,
        Err(StoreError::InvalidCredentials)
    ));
    assert!(matches!(
        store.user_by_session_token(&token).await,
        Err(StoreError::Unauthorized)
    ));
}

#[tokio::test]
async fn expired_public_session_candidates_are_retryable_until_access_is_deleted() {
    let store = SessionStore::in_memory_for_tests();
    let session_id = Uuid::new_v4().to_string();
    store
        .grant_public_session_access("public-token", &session_id, 0)
        .await
        .expect("expired access row can be inserted");

    assert_eq!(
        store
            .expired_public_session_ids()
            .await
            .expect("expired sessions can be listed"),
        vec![session_id.clone()]
    );
    assert_eq!(
        store
            .expired_public_session_ids()
            .await
            .expect("expired sessions remain retryable"),
        vec![session_id.clone()]
    );

    store
        .delete_public_session_access_for_session(&session_id)
        .await
        .expect("public session access can be deleted after cleanup");
    assert!(store
        .expired_public_session_ids()
        .await
        .expect("expired sessions can be listed after delete")
        .is_empty());
}
