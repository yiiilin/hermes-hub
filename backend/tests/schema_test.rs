use hermes_hub_backend::db::migrations::schema_migrations;
use hermes_hub_backend::security::crypto::{decrypt_secret, encrypt_secret, SecretCipher};

fn schema_migrations_include_initial_tables() {
    let sql = schema_migrations();

    assert!(sql.contains("create table if not exists users"));
    assert!(sql.contains("create table if not exists invites"));
    assert!(sql.contains("create table if not exists hermes_instances"));
    assert!(sql.contains("create table if not exists llm_usage_events"));
    assert!(sql.contains("create table if not exists channels"));
    assert!(sql.contains("create table if not exists channel_sessions"));
    assert!(sql.contains("create table if not exists channel_session_messages"));
    assert!(sql.contains("create table if not exists channel_attachments"));
    assert!(sql.contains("create table if not exists channel_runs"));
    assert!(sql.contains("status text not null default 'queued'"));
    assert!(sql.contains("channel_runs_ready_idx"));
    assert!(sql.contains("channel_runs_user_message_idx"));
    assert!(sql.contains("client_message_key text"));
    assert!(sql.contains("channel_session_messages_client_key_idx"));
    assert!(
        !sql.contains("\n    base_url text not null,"),
        "Hermes instances no longer store an inbound base URL"
    );
}

fn secret_cipher_round_trips_plaintext() {
    let cipher = SecretCipher::from_master_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .or_else(|_| SecretCipher::from_master_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
        .expect("master key decodes");
    let encrypted = encrypt_secret(&cipher, "provider-key-123");
    let decrypted = decrypt_secret(&cipher, &encrypted).expect("secret decrypts");

    assert_eq!(decrypted, "provider-key-123");
}

#[test]
fn schema_test() {
    schema_migrations_include_initial_tables();
    secret_cipher_round_trips_plaintext();
}
