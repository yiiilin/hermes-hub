use argon2::{
    password_hash::{
        Error as PasswordHashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2,
};
use serde::Serialize;
use thiserror::Error;

/// A platform user role. The first registered account is always `admin`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    User,
}

/// Account lifecycle status used by auth middleware.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
}

/// Internal user record kept by the MVP store.
#[derive(Clone, Debug)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Public user payload returned by HTTP APIs.
#[derive(Clone, Debug, Serialize)]
pub struct PublicUser {
    pub id: String,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("password hash failed")]
    HashFailed,
    #[error("password verify failed")]
    VerifyFailed,
}

impl User {
    pub fn public(&self) -> PublicUser {
        PublicUser {
            id: self.id.clone(),
            email: self.email.clone(),
            role: self.role.clone(),
            status: self.status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Hash a password with Argon2id and a random salt.
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    let mut salt_bytes = [0u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|_| PasswordError::HashFailed)?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| PasswordError::HashFailed)?;

    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::HashFailed)
}

/// Verify a plaintext password against a stored Argon2id password hash.
pub fn verify_password(stored_hash: &str, password: &str) -> Result<bool, PasswordError> {
    let parsed_hash = PasswordHash::new(stored_hash).map_err(|_| PasswordError::VerifyFailed)?;

    match Argon2::default().verify_password(password.as_bytes(), &parsed_hash) {
        Ok(()) => Ok(true),
        Err(PasswordHashError::Password) => Ok(false),
        Err(_) => Err(PasswordError::VerifyFailed),
    }
}
