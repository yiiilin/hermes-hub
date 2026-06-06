use hermes_hub_backend::db::migrations::schema_migrations;
use hermes_hub_backend::security::crypto::{decrypt_secret, encrypt_secret, SecretCipher};

fn schema_migrations_include_initial_tables() {
    let sql = schema_migrations();

    assert!(sql.contains("create table if not exists users"));
    assert!(sql.contains("auth_provider text not null default 'local'"));
    assert!(sql.contains("add column if not exists auth_provider text not null default 'legacy'"));
    assert!(sql.contains("alter column auth_provider set default 'local'"));
    assert!(sql.contains("add column if not exists purpose text not null default 'web'"));
    assert!(sql.contains("users_auth_provider_check"));
    assert!(sql.contains("create table if not exists business_oauth_authorization_codes"));
    assert!(sql.contains("create table if not exists invites"));
    assert!(sql.contains("create table if not exists hermes_instances"));
    assert!(sql.contains("create table if not exists llm_usage_events"));
    assert!(sql.contains("create table if not exists channels"));
    assert!(sql.contains("create table if not exists channel_sessions"));
    assert!(sql.contains("add column if not exists hidden_from_web boolean not null default false"));
    assert!(sql.contains("api_management"));
    assert!(sql.contains("create table if not exists channel_session_messages"));
    assert!(sql.contains("create table if not exists channel_attachments"));
    assert!(sql.contains("create table if not exists channel_runs"));
    assert!(sql.contains("status text not null default 'queued'"));
    assert!(sql.contains("channel_runs_ready_idx"));
    assert!(sql.contains("channel_runs_user_message_idx"));
    assert!(sql.contains("create table if not exists hermes_scheduler_snapshots"));
    assert!(sql.contains("scheduler_status text not null default 'unavailable'"));
    assert!(sql.contains("tasks jsonb not null default '[]'::jsonb"));
    assert!(
        sql.contains("alter table hermes_instances add column if not exists last_user_activity_at")
    );
    assert!(sql.contains("alter table hermes_instances add column if not exists last_started_at"));
    assert!(sql.contains("alter table hermes_instances add column if not exists last_stopped_at"));
    assert!(sql.contains("alter table hermes_instances add column if not exists stopped_reason"));
    assert!(sql.contains("client_message_key text"));
    assert!(sql.contains("channel_session_messages_client_key_idx"));
    assert!(sql.contains("context_window_tokens bigint not null default 128000"));
    assert!(
        sql.contains("alter table model_configs add column if not exists context_window_tokens")
    );
    assert!(sql.contains("max_output_tokens bigint not null default 4096"));
    assert!(sql.contains("temperature double precision not null default 0.7"));
    assert!(sql.contains("supports_parallel_tools boolean not null default true"));
    assert!(sql.contains("fallback_config jsonb"));
    assert!(sql.contains("alter table model_configs add column if not exists fallback_config"));
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
