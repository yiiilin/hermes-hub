use hermes_hub_backend::channel::service::{ChannelMessageRole, ChannelSessionKind, ChannelStore};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn memory_messages_expose_and_refresh_updated_at() {
    let store = ChannelStore::default();
    let user_id = "user-message-time";
    let channel = store
        .ensure_hub_channel(user_id)
        .await
        .expect("channel can be ensured");
    let session = store
        .create_session(user_id, &channel.id, ChannelSessionKind::Chat, None, false)
        .await
        .expect("session can be created");
    let created = store
        .append_session_message(
            user_id,
            &channel.id,
            &session.id,
            ChannelMessageRole::Assistant,
            None,
            "first".to_string(),
            json!([]),
        )
        .await
        .expect("message can be appended");
    let created_json = serde_json::to_value(&created).expect("message serializes");
    let created_updated_at = json_u64(&created_json, "updated_at");

    sleep(Duration::from_millis(1_100)).await;

    let updated = store
        .update_session_message(
            user_id,
            &channel.id,
            &session.id,
            &created.id,
            "second".to_string(),
            json!([]),
        )
        .await
        .expect("message can be updated");
    let updated_json = serde_json::to_value(&updated).expect("message serializes");

    assert_eq!(
        json_u64(&created_json, "created_at"),
        json_u64(&updated_json, "created_at")
    );
    assert!(
        json_u64(&updated_json, "updated_at") > created_updated_at,
        "updated_at should reflect the latest message update"
    );
}

fn json_u64(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{key} should be present as an integer"))
}
