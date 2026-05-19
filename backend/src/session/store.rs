use crate::domain::{
    invite::{Invite, InviteStatus, PublicInvite},
    user::{hash_password, verify_password, User, UserRole, UserStatus},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

const SESSION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Small in-memory store for the MVP auth flow.
///
/// This keeps Task 3 focused while preserving method boundaries that can later
/// be moved to SQLx/PostgreSQL without changing HTTP handlers.
#[derive(Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<StoreInner>>,
}

#[derive(Default)]
struct StoreInner {
    users_by_id: HashMap<String, User>,
    user_ids_by_email: HashMap<String, String>,
    sessions_by_hash: HashMap<String, StoredSession>,
    invites_by_id: HashMap<String, Invite>,
    invite_ids_by_hash: HashMap<String, String>,
}

#[derive(Clone)]
struct StoredSession {
    user_id: String,
    expires_at: u64,
}

#[derive(Clone, Debug)]
pub struct CreatedInvite {
    pub token: String,
    pub invite: PublicInvite,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("bootstrap registration is already closed")]
    BootstrapClosed,
    #[error("email is already registered")]
    EmailAlreadyRegistered,
    #[error("invalid email or password")]
    InvalidCredentials,
    #[error("unauthorized")]
    Unauthorized,
    #[error("invite not found")]
    InviteNotFound,
    #[error("invite expired")]
    InviteExpired,
    #[error("invite exhausted")]
    InviteExhausted,
    #[error("invite revoked")]
    InviteRevoked,
    #[error("invite not found")]
    InviteIdNotFound,
    #[error("password operation failed")]
    PasswordFailed,
    #[error("store lock failed")]
    LockFailed,
}

impl SessionStore {
    pub fn create_bootstrap_admin(&self, email: &str, password: &str) -> Result<User, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;

        if !inner.users_by_id.is_empty() {
            return Err(StoreError::BootstrapClosed);
        }

        inner.create_user(email, password, UserRole::Admin)
    }

    pub fn register_with_invite(
        &self,
        invite_token: &str,
        email: &str,
        password: &str,
    ) -> Result<User, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let now = unix_now();
        let token_hash = hash_token(invite_token);
        let invite_id = inner
            .invite_ids_by_hash
            .get(&token_hash)
            .cloned()
            .ok_or(StoreError::InviteNotFound)?;

        let invite = inner
            .invites_by_id
            .get_mut(&invite_id)
            .ok_or(StoreError::InviteNotFound)?;

        if invite.status == InviteStatus::Revoked {
            return Err(StoreError::InviteRevoked);
        }

        if invite.expires_at <= now {
            invite.status = InviteStatus::Expired;
            invite.updated_at = now;
            return Err(StoreError::InviteExpired);
        }

        if invite.used_count >= invite.max_uses || invite.status == InviteStatus::Exhausted {
            invite.status = InviteStatus::Exhausted;
            invite.updated_at = now;
            return Err(StoreError::InviteExhausted);
        }

        if inner
            .user_ids_by_email
            .contains_key(&normalize_email(email))
        {
            return Err(StoreError::EmailAlreadyRegistered);
        }

        let user = inner.create_user(email, password, UserRole::User)?;
        let invite = inner
            .invites_by_id
            .get_mut(&invite_id)
            .ok_or(StoreError::InviteNotFound)?;
        invite.used_count += 1;
        invite.updated_at = now;

        if invite.used_count >= invite.max_uses {
            invite.status = InviteStatus::Exhausted;
        }

        Ok(user)
    }

    pub fn login(&self, email: &str, password: &str) -> Result<User, StoreError> {
        let inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let email = normalize_email(email);
        let user_id = inner
            .user_ids_by_email
            .get(&email)
            .ok_or(StoreError::InvalidCredentials)?;
        let user = inner
            .users_by_id
            .get(user_id)
            .ok_or(StoreError::InvalidCredentials)?;

        if user.status != UserStatus::Active {
            return Err(StoreError::InvalidCredentials);
        }

        let verified = verify_password(&user.password_hash, password)
            .map_err(|_| StoreError::PasswordFailed)?;

        if !verified {
            return Err(StoreError::InvalidCredentials);
        }

        Ok(user.clone())
    }

    pub fn create_session(&self, user_id: &str) -> Result<String, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let token = random_token();
        let session = StoredSession {
            user_id: user_id.to_string(),
            expires_at: unix_now() + SESSION_TTL_SECONDS,
        };

        inner.sessions_by_hash.insert(hash_token(&token), session);
        Ok(token)
    }

    pub fn user_by_session_token(&self, token: &str) -> Result<User, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let token_hash = hash_token(token);
        let now = unix_now();
        let session = inner
            .sessions_by_hash
            .get(&token_hash)
            .cloned()
            .ok_or(StoreError::Unauthorized)?;

        if session.expires_at <= now {
            inner.sessions_by_hash.remove(&token_hash);
            return Err(StoreError::Unauthorized);
        }

        inner
            .users_by_id
            .get(&session.user_id)
            .filter(|user| user.status == UserStatus::Active)
            .cloned()
            .ok_or(StoreError::Unauthorized)
    }

    pub fn delete_session(&self, token: &str) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        inner.sessions_by_hash.remove(&hash_token(token));
        Ok(())
    }

    pub fn create_invite(
        &self,
        created_by_user_id: &str,
        expires_at: u64,
        max_uses: u32,
    ) -> Result<CreatedInvite, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let now = unix_now();
        let token = random_token();
        let invite = Invite {
            id: Uuid::new_v4().to_string(),
            token_hash: hash_token(&token),
            created_by_user_id: created_by_user_id.to_string(),
            status: InviteStatus::Pending,
            expires_at,
            max_uses,
            used_count: 0,
            created_at: now,
            updated_at: now,
        };
        let public = invite.public();

        inner
            .invite_ids_by_hash
            .insert(invite.token_hash.clone(), invite.id.clone());
        inner.invites_by_id.insert(invite.id.clone(), invite);

        Ok(CreatedInvite {
            token,
            invite: public,
        })
    }

    pub fn list_invites(&self) -> Result<Vec<PublicInvite>, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let now = unix_now();
        let mut invites = inner
            .invites_by_id
            .values_mut()
            .map(|invite| {
                if invite.status == InviteStatus::Pending && invite.expires_at <= now {
                    invite.status = InviteStatus::Expired;
                    invite.updated_at = now;
                }
                invite.public()
            })
            .collect::<Vec<_>>();

        invites.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(invites)
    }

    pub fn revoke_invite(&self, invite_id: &str) -> Result<PublicInvite, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::LockFailed)?;
        let now = unix_now();
        let invite = inner
            .invites_by_id
            .get_mut(invite_id)
            .ok_or(StoreError::InviteIdNotFound)?;

        invite.status = InviteStatus::Revoked;
        invite.updated_at = now;
        Ok(invite.public())
    }
}

impl StoreInner {
    fn create_user(
        &mut self,
        email: &str,
        password: &str,
        role: UserRole,
    ) -> Result<User, StoreError> {
        let email = normalize_email(email);

        if self.user_ids_by_email.contains_key(&email) {
            return Err(StoreError::EmailAlreadyRegistered);
        }

        let now = unix_now();
        let user = User {
            id: Uuid::new_v4().to_string(),
            email: email.clone(),
            password_hash: hash_password(password).map_err(|_| StoreError::PasswordFailed)?,
            role,
            status: UserStatus::Active,
            created_at: now,
            updated_at: now,
        };

        self.user_ids_by_email.insert(email, user.id.clone());
        self.users_by_id.insert(user.id.clone(), user.clone());
        Ok(user)
    }
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("os rng must be available");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}
