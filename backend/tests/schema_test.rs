use hermes_hub_backend::db::migrations::schema_migrations;
use hermes_hub_backend::security::crypto::{decrypt_secret, encrypt_secret, SecretCipher};

fn schema_migrations_include_initial_tables() {
    let sql = schema_migrations();

    assert!(sql.contains("create table if not exists users"));
    assert!(sql.contains("create table if not exists invites"));
    assert!(sql.contains("create table if not exists hermes_instances"));
    assert!(sql.contains("create table if not exists llm_usage_events"));
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
