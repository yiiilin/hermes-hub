use crate::domain::{
    invite::{Invite, InviteStatus, PublicInvite},
    user::{hash_password, verify_password, User, UserListItem, UserRole, UserStatus},
};
use crate::hermes::instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus};
use crate::{
    db::runtime::block_on_db,
    security::crypto::{decrypt_secret, encrypt_secret, SecretCipher},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Postgres, Row};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

const SESSION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_MAX_SESSIONS_PER_USER: u32 = 20;
const MAX_CONFIGURABLE_SESSIONS_PER_USER: u32 = 500;
const MAX_SESSIONS_PER_USER_KEY: &str = "max_sessions_per_user";
const OIDC_SETTINGS_KEY: &str = "oidc";

#[derive(Clone)]
pub struct SessionStore {
    backend: SessionStoreBackend,
}

#[derive(Clone)]
enum SessionStoreBackend {
    Memory(Arc<Mutex<StoreInner>>),
    Postgres { pool: PgPool, cipher: SecretCipher },
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            backend: SessionStoreBackend::Memory(Arc::new(Mutex::new(StoreInner::default()))),
        }
    }
}

struct StoreInner {
    users_by_id: HashMap<String, User>,
    user_ids_by_email: HashMap<String, String>,
    sessions_by_hash: HashMap<String, StoredSession>,
    invites_by_id: HashMap<String, Invite>,
    invite_ids_by_hash: HashMap<String, String>,
    hermes_instances_by_user_id: HashMap<String, HermesInstance>,
    hermes_scheduler_snapshots_by_instance_id: HashMap<String, HermesSchedulerSnapshot>,
    hermes_lifecycle_by_instance_id: HashMap<String, HermesLifecycleState>,
    proxy_audit_logs: Vec<ProxyAuditEvent>,
    llm_usage_events: Vec<LlmUsageEvent>,
    system_settings: SystemSettings,
}

impl Default for StoreInner {
    fn default() -> Self {
        Self {
            users_by_id: HashMap::new(),
            user_ids_by_email: HashMap::new(),
            sessions_by_hash: HashMap::new(),
            invites_by_id: HashMap::new(),
            invite_ids_by_hash: HashMap::new(),
            hermes_instances_by_user_id: HashMap::new(),
            hermes_scheduler_snapshots_by_instance_id: HashMap::new(),
            hermes_lifecycle_by_instance_id: HashMap::new(),
            proxy_audit_logs: Vec::new(),
            llm_usage_events: Vec::new(),
            system_settings: SystemSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
pub struct SystemSettings {
    pub max_sessions_per_user: u32,
    #[serde(default)]
    pub oidc: OidcSettings,
}

/// 管理员可配置的 OIDC 参数。字段名尽量贴近 Outline 的环境变量语义，
/// 但放在系统设置中，便于运行时调整。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OidcSettings {
    pub enabled: bool,
    pub display_name: String,
    pub client_id: String,
    pub client_secret: String,
    pub issuer_url: String,
    pub authorization_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub logout_url: String,
    pub scopes: String,
    pub username_claim: String,
    pub email_claim: String,
    pub allow_password_login: bool,
    pub auto_create_users: bool,
}

/// OIDC 登录会复用已有用户，也可能按配置自动创建新用户。
/// HTTP 层需要知道是否新建，才能只在创建用户后补建 Hermes 运行时。
#[derive(Clone, Debug)]
pub struct OidcUserResult {
    pub user: User,
    pub created: bool,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            max_sessions_per_user: DEFAULT_MAX_SESSIONS_PER_USER,
            oidc: OidcSettings::default(),
        }
    }
}

impl Default for OidcSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            display_name: "OpenID Connect".to_string(),
            client_id: String::new(),
            client_secret: String::new(),
            issuer_url: String::new(),
            authorization_url: String::new(),
            token_url: String::new(),
            userinfo_url: String::new(),
            logout_url: String::new(),
            scopes: "openid profile email".to_string(),
            username_claim: "preferred_username".to_string(),
            email_claim: "email".to_string(),
            allow_password_login: true,
            auto_create_users: true,
        }
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyAuditEvent {
    pub user_id: Option<String>,
    pub hermes_instance_id: Option<String>,
    pub direction: String,
    pub method: String,
    pub path: String,
    pub status_code: Option<u16>,
    pub duration_ms: Option<u64>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmUsageEvent {
    pub user_id: Option<String>,
    pub hermes_instance_id: Option<String>,
    pub model: String,
    pub upstream_provider: String,
    pub status_code: Option<u16>,
    pub duration_ms: Option<u64>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HermesScheduledTaskSnapshot {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedule: String,
    pub timezone: String,
    pub next_run_at: Option<u64>,
    pub last_run_at: Option<u64>,
    pub status: String,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HermesSchedulerSnapshot {
    pub user_id: String,
    pub user_email: Option<String>,
    pub hermes_instance_id: String,
    pub instance_status: String,
    pub scheduler_status: String,
    pub scheduler_enabled: bool,
    pub running_jobs_count: u32,
    pub reported_at: u64,
    pub source: String,
    pub snapshot_hash: Option<String>,
    pub next_wake_at: Option<u64>,
    pub tasks: Vec<HermesScheduledTaskSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesSchedulerSnapshotInput {
    pub scheduler_status: String,
    pub scheduler_enabled: bool,
    pub running_jobs_count: u32,
    pub reported_at: u64,
    pub source: String,
    pub snapshot_hash: Option<String>,
    pub next_wake_at: Option<u64>,
    pub tasks: Vec<HermesScheduledTaskSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesLifecycleState {
    pub instance_id: String,
    pub user_id: String,
    pub last_user_activity_at: Option<u64>,
    pub last_started_at: Option<u64>,
    pub last_stopped_at: Option<u64>,
    pub stopped_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesLifecycleCandidate {
    pub instance: HermesInstance,
    pub lifecycle: HermesLifecycleState,
    pub scheduler_snapshot: Option<HermesSchedulerSnapshot>,
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
    #[error("database operation failed")]
    DatabaseFailed,
    #[error("secret operation failed")]
    SecretFailed,
    #[error("invalid system settings")]
    InvalidSystemSettings,
}

impl SessionStore {
    pub fn postgres(pool: PgPool, cipher: SecretCipher) -> Self {
        Self {
            backend: SessionStoreBackend::Postgres { pool, cipher },
        }
    }

    /// 判断首个管理员注册是否仍然开放。
    ///
    /// 这个状态只用于未登录页面决定默认展示登录还是注册；真正的并发安全仍由
    /// `create_bootstrap_admin` 内部的锁和事务保证。
    pub async fn bootstrap_open(&self) -> Result<bool, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                Ok(inner.users_by_id.is_empty())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let count = sqlx::query("select count(*)::bigint as count from users")
                    .fetch_one(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .try_get::<i64, _>("count")
                    .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(count == 0)
            }),
        }
    }

    pub async fn create_bootstrap_admin(
        &self,
        email: &str,
        password: &str,
    ) -> Result<User, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;

                if !inner.users_by_id.is_empty() {
                    return Err(StoreError::BootstrapClosed);
                }

                inner.create_user(email, password, UserRole::Admin)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let mut tx = pool.begin().await.map_err(|_| StoreError::DatabaseFailed)?;
                sqlx::query("select pg_advisory_xact_lock(hashtext('hermes_hub_bootstrap_admin'))")
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?;
                let count = sqlx::query("select count(*)::bigint as count from users")
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .try_get::<i64, _>("count")
                    .map_err(|_| StoreError::DatabaseFailed)?;

                if count > 0 {
                    return Err(StoreError::BootstrapClosed);
                }

                let user =
                    postgres_create_user_with_executor(&mut *tx, email, password, UserRole::Admin)
                        .await?;
                tx.commit().await.map_err(|_| StoreError::DatabaseFailed)?;
                Ok(user)
            }),
        }
    }

    pub async fn register_with_invite(
        &self,
        invite_token: &str,
        email: &str,
        password: &str,
    ) -> Result<User, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                register_with_invite_in_memory(&mut inner, invite_token, email, password)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let mut tx = pool.begin().await.map_err(|_| StoreError::DatabaseFailed)?;
                let now = unix_now();
                let token_hash = hash_token(invite_token);
                let invite_sql = invite_select("select", "where token_hash = $1 for update", "");
                let invite_row = sqlx::query(&invite_sql)
                    .bind(token_hash)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .ok_or(StoreError::InviteNotFound)?;
                let invite = row_to_invite(&invite_row)?;

                if invite.status == InviteStatus::Revoked {
                    return Err(StoreError::InviteRevoked);
                }
                if invite.expires_at <= now {
                    mark_invite_status_with_executor(&mut *tx, &invite.id, InviteStatus::Expired)
                        .await?;
                    tx.commit().await.map_err(|_| StoreError::DatabaseFailed)?;
                    return Err(StoreError::InviteExpired);
                }
                if invite.used_count >= invite.max_uses || invite.status == InviteStatus::Exhausted
                {
                    mark_invite_status_with_executor(&mut *tx, &invite.id, InviteStatus::Exhausted)
                        .await?;
                    tx.commit().await.map_err(|_| StoreError::DatabaseFailed)?;
                    return Err(StoreError::InviteExhausted);
                }
                if postgres_user_id_by_email_with_executor(&mut *tx, email)
                    .await?
                    .is_some()
                {
                    return Err(StoreError::EmailAlreadyRegistered);
                }

                let user =
                    postgres_create_user_with_executor(&mut *tx, email, password, UserRole::User)
                        .await?;
                let next_used_count = invite.used_count + 1;
                let next_status = if next_used_count >= invite.max_uses {
                    "exhausted"
                } else {
                    "pending"
                };
                sqlx::query(
                    r#"
                    update invites
                    set used_count = used_count + 1,
                        status = $2,
                        updated_at = now()
                    where id = $1::uuid
                    "#,
                )
                .bind(&invite.id)
                .bind(next_status)
                .execute(&mut *tx)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;
                sqlx::query(
                    "insert into invite_uses (id, invite_id, used_by_user_id) values ($1::uuid, $2::uuid, $3::uuid)",
                )
                .bind(Uuid::new_v4().to_string())
                .bind(&invite.id)
                .bind(&user.id)
                .execute(&mut *tx)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;
                tx.commit().await.map_err(|_| StoreError::DatabaseFailed)?;

                Ok(user)
            }),
        }
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<User, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                login_in_memory(&inner, email, password)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let user = postgres_user_by_email(pool, email)
                    .await?
                    .ok_or(StoreError::InvalidCredentials)?;

                if user.status != UserStatus::Active {
                    return Err(StoreError::InvalidCredentials);
                }

                let verified = verify_password(&user.password_hash, password)
                    .map_err(|_| StoreError::PasswordFailed)?;
                if !verified {
                    return Err(StoreError::InvalidCredentials);
                }

                Ok(user)
            }),
        }
    }

    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let email = normalize_email(email);
                Ok(inner
                    .user_ids_by_email
                    .get(&email)
                    .and_then(|user_id| inner.users_by_id.get(user_id))
                    .cloned())
            }
            SessionStoreBackend::Postgres { pool, .. } => postgres_user_by_email(pool, email).await,
        }
    }

    pub async fn get_or_create_oidc_user(
        &self,
        email: &str,
        auto_create: bool,
    ) -> Result<OidcUserResult, StoreError> {
        if let Some(user) = self.user_by_email(email).await? {
            if user.status == UserStatus::Active {
                return Ok(OidcUserResult {
                    user,
                    created: false,
                });
            }
            return Err(StoreError::InvalidCredentials);
        }

        if !auto_create {
            return Err(StoreError::InvalidCredentials);
        }

        // OIDC 用户不使用密码登录；这里生成不可猜测占位密码哈希，保持现有 users 表结构。
        let placeholder_password = format!("oidc:{}", Uuid::new_v4());
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let user = inner.create_user(email, &placeholder_password, UserRole::User)?;
                Ok(OidcUserResult {
                    user,
                    created: true,
                })
            }
            SessionStoreBackend::Postgres { pool, .. } => {
                let user = postgres_create_user(pool, email, &placeholder_password, UserRole::User)
                    .await?;
                Ok(OidcUserResult {
                    user,
                    created: true,
                })
            }
        }
    }

    pub async fn create_session(&self, user_id: &str) -> Result<String, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let token = random_token();
                let session = StoredSession {
                    user_id: user_id.to_string(),
                    expires_at: unix_now() + SESSION_TTL_SECONDS,
                };

                inner.sessions_by_hash.insert(hash_token(&token), session);
                Ok(token)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let token = random_token();
                let expires_at = unix_now() + SESSION_TTL_SECONDS;
                sqlx::query(
                    r#"
                    insert into sessions (id, user_id, session_token_hash, expires_at)
                    values ($1::uuid, $2::uuid, $3, to_timestamp($4))
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(user_id)
                .bind(hash_token(&token))
                .bind(expires_at as f64)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(token)
            }),
        }
    }

    pub async fn user_by_session_token(&self, token: &str) -> Result<User, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                user_by_session_token_in_memory(&mut inner, token)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let token_hash = hash_token(token);
                let row = sqlx::query(
                    r#"
                    select extract(epoch from sessions.expires_at)::bigint as session_expires_at,
                           users.id::text as id,
                           users.email,
                           users.password_hash,
                           users.role,
                           users.status,
                           extract(epoch from users.created_at)::bigint as created_at,
                           extract(epoch from users.updated_at)::bigint as updated_at
                    from sessions
                    join users on users.id = sessions.user_id
                    where sessions.session_token_hash = $1
                    "#,
                )
                .bind(&token_hash)
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?
                .ok_or(StoreError::Unauthorized)?;
                let expires_at =
                    row.try_get::<i64, _>("session_expires_at")
                        .map_err(|_| StoreError::DatabaseFailed)? as u64;

                if expires_at <= unix_now() {
                    sqlx::query("delete from sessions where session_token_hash = $1")
                        .bind(token_hash)
                        .execute(pool)
                        .await
                        .map_err(|_| StoreError::DatabaseFailed)?;
                    return Err(StoreError::Unauthorized);
                }

                let user = row_to_user(&row)?;
                if user.status != UserStatus::Active {
                    return Err(StoreError::Unauthorized);
                }

                Ok(user)
            }),
        }
    }

    pub async fn delete_session(&self, token: &str) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                inner.sessions_by_hash.remove(&hash_token(token));
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query("delete from sessions where session_token_hash = $1")
                    .bind(hash_token(token))
                    .execute(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?;
                Ok(())
            }),
        }
    }

    pub async fn create_invite(
        &self,
        created_by_user_id: &str,
        expires_at: u64,
        max_uses: u32,
    ) -> Result<CreatedInvite, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                create_invite_in_memory(&mut inner, created_by_user_id, expires_at, max_uses)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let token = random_token();
                let invite = Invite {
                    id: Uuid::new_v4().to_string(),
                    token_hash: hash_token(&token),
                    created_by_user_id: created_by_user_id.to_string(),
                    status: InviteStatus::Pending,
                    expires_at,
                    max_uses,
                    used_count: 0,
                    created_at: unix_now(),
                    updated_at: unix_now(),
                };
                sqlx::query(
                    r#"
                    insert into invites (
                        id, token_hash, created_by_user_id, status,
                        expires_at, max_uses, used_count, created_at, updated_at
                    )
                    values ($1::uuid, $2, $3::uuid, 'pending',
                            to_timestamp($4), $5, 0, to_timestamp($6), to_timestamp($7))
                    "#,
                )
                .bind(&invite.id)
                .bind(&invite.token_hash)
                .bind(&invite.created_by_user_id)
                .bind(expires_at as f64)
                .bind(max_uses as i32)
                .bind(invite.created_at as f64)
                .bind(invite.updated_at as f64)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(CreatedInvite {
                    token,
                    invite: invite.public(),
                })
            }),
        }
    }

    pub async fn list_invites(&self) -> Result<Vec<PublicInvite>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                list_invites_in_memory(&mut inner)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query(
                    "update invites set status = 'expired', updated_at = now() where status = 'pending' and expires_at <= now()",
                )
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                let invite_sql = invite_select("select", "", "order by created_at desc");
                let rows = sqlx::query(&invite_sql)
                    .fetch_all(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?;

                rows.iter()
                    .map(row_to_invite)
                    .map(|invite| invite.map(|invite| invite.public()))
                    .collect()
            }),
        }
    }

    pub async fn revoke_invite(&self, invite_id: &str) -> Result<PublicInvite, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                revoke_invite_in_memory(&mut inner, invite_id)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update invites
                    set status = 'revoked', updated_at = now()
                    where id = $1::uuid
                    returning id::text as id,
                              token_hash,
                              coalesce(created_by_user_id::text, '') as created_by_user_id,
                              status,
                              extract(epoch from expires_at)::bigint as expires_at,
                              max_uses,
                              used_count,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(invite_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?
                .ok_or(StoreError::InviteIdNotFound)?;

                Ok(row_to_invite(&row)?.public())
            }),
        }
    }

    pub async fn list_users(&self) -> Result<Vec<UserListItem>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let mut users = inner
                    .users_by_id
                    .values()
                    .map(User::list_item)
                    .collect::<Vec<_>>();

                users.sort_by(|left, right| right.created_at.cmp(&left.created_at));
                Ok(users)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let rows = sqlx::query(
                    r#"
                    select id::text as id,
                           email,
                           password_hash,
                           role,
                           status,
                           extract(epoch from created_at)::bigint as created_at,
                           extract(epoch from updated_at)::bigint as updated_at
                    from users
                    order by created_at desc
                    "#,
                )
                .fetch_all(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                rows.iter()
                    .map(row_to_user)
                    .map(|user| user.map(|user| user.list_item()))
                    .collect()
            }),
        }
    }

    pub async fn disable_user(&self, user_id: &str) -> Result<User, StoreError> {
        self.set_user_status(user_id, UserStatus::Disabled).await
    }

    pub async fn enable_user(&self, user_id: &str) -> Result<User, StoreError> {
        self.set_user_status(user_id, UserStatus::Active).await
    }

    pub async fn bind_hermes_instance(&self, instance: HermesInstance) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                update_memory_lifecycle_from_instance(&mut inner, &instance, None);
                inner
                    .hermes_instances_by_user_id
                    .insert(instance.user_id.clone(), instance);
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let encrypted_token = instance
                    .api_token_secret_ref
                    .as_ref()
                    .map(|token| encrypt_secret(cipher, token));
                sqlx::query(
                    r#"
                    insert into hermes_instances (
                        id, user_id, kind, status, name, api_token_secret_ref,
                        container_id, host_workspace_path, host_sandbox_path, host_config_path,
                        health_status, status_message, runtime_image, runtime_version,
                        last_started_at, last_stopped_at, stopped_reason, updated_at
                    )
                    values (
                        $1::uuid, $2::uuid, $3, $4, $5, $6,
                        $7, $8, $9, $10, $11, $12, $13, $14,
                        case when $4 = 'running' then now() else null end,
                        case when $4 = 'stopped' then now() else null end,
                        case when $4 = 'stopped' then 'manual' else null end,
                        now()
                    )
                    on conflict (user_id) do update
                    set id = excluded.id,
                        kind = excluded.kind,
                        status = excluded.status,
                        name = excluded.name,
                        api_token_secret_ref = excluded.api_token_secret_ref,
                        container_id = excluded.container_id,
                        host_workspace_path = excluded.host_workspace_path,
                        host_sandbox_path = excluded.host_sandbox_path,
                        host_config_path = excluded.host_config_path,
                        health_status = excluded.health_status,
                        status_message = excluded.status_message,
                        runtime_image = excluded.runtime_image,
                        runtime_version = excluded.runtime_version,
                        last_started_at = case
                            when excluded.status = 'running' and hermes_instances.status <> 'running' then now()
                            else coalesce(hermes_instances.last_started_at, excluded.last_started_at)
                        end,
                        last_stopped_at = case
                            when excluded.status = 'stopped' and hermes_instances.status <> 'stopped' then now()
                            else hermes_instances.last_stopped_at
                        end,
                        stopped_reason = case
                            when excluded.status = 'running' then null
                            when excluded.status = 'stopped' and hermes_instances.status <> 'stopped' then coalesce(hermes_instances.stopped_reason, 'manual')
                            else hermes_instances.stopped_reason
                        end,
                        updated_at = now()
                    "#,
                )
                .bind(&instance.id)
                .bind(&instance.user_id)
                .bind(hermes_kind_as_str(&instance.kind))
                .bind(hermes_status_as_str(&instance.status))
                .bind(&instance.name)
                .bind(encrypted_token)
                .bind(&instance.container_id)
                .bind(&instance.host_workspace_path)
                .bind(&instance.host_sandbox_path)
                .bind(&instance.host_config_path)
                .bind(&instance.health_status)
                .bind(&instance.status_message)
                .bind(&instance.runtime_image)
                .bind(&instance.runtime_version)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(())
            }),
        }
    }

    pub async fn hermes_instance_for_user(
        &self,
        user_id: &str,
    ) -> Result<HermesInstance, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                inner
                    .hermes_instances_by_user_id
                    .get(user_id)
                    .cloned()
                    .ok_or(StoreError::InviteNotFound)
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let hermes_sql = hermes_instance_select("select", "where user_id = $1::uuid", "");
                let row = sqlx::query(&hermes_sql)
                    .bind(user_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .ok_or(StoreError::InviteNotFound)?;

                row_to_hermes_instance(&row, cipher)
            }),
        }
    }

    pub async fn list_hermes_instances(&self) -> Result<Vec<HermesInstance>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let mut instances = inner
                    .hermes_instances_by_user_id
                    .values()
                    .filter(|instance| instance.kind == HermesInstanceKind::ManagedDocker)
                    .cloned()
                    .collect::<Vec<_>>();
                instances.sort_by(|left, right| left.user_id.cmp(&right.user_id));
                Ok(instances)
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let hermes_sql = hermes_instance_select(
                    "select",
                    "where kind = 'managed_docker'",
                    "order by user_id",
                );
                let rows = sqlx::query(&hermes_sql)
                    .fetch_all(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?;

                rows.iter()
                    .map(|row| row_to_hermes_instance(row, cipher))
                    .collect()
            }),
        }
    }

    pub async fn set_hermes_instance_status(
        &self,
        user_id: &str,
        status: HermesInstanceStatus,
    ) -> Result<HermesInstance, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let instance = inner
                    .hermes_instances_by_user_id
                    .get_mut(user_id)
                    .ok_or(StoreError::InviteNotFound)?;
                instance.status = status;
                Ok(instance.clone())
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update hermes_instances
                    set status = $2, updated_at = now()
                    where user_id = $1::uuid
                    returning id::text as id,
                              user_id::text as user_id,
                              kind,
                              status,
                              name,
                              api_token_secret_ref,
                              container_id,
                              host_workspace_path,
                              host_sandbox_path,
                              host_config_path,
                              health_status,
                              status_message,
                              runtime_image,
                              runtime_version
                    "#,
                )
                .bind(user_id)
                .bind(hermes_status_as_str(&status))
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?
                .ok_or(StoreError::InviteNotFound)?;

                row_to_hermes_instance(&row, cipher)
            }),
        }
    }

    pub async fn update_hermes_instance_runtime(
        &self,
        instance_id: &str,
        runtime_image: Option<String>,
        runtime_version: Option<String>,
    ) -> Result<HermesInstance, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let instance = inner
                    .hermes_instances_by_user_id
                    .values_mut()
                    .find(|instance| instance.id == instance_id)
                    .ok_or(StoreError::InviteNotFound)?;
                // adapter 上报是运行态事实；空字段表示本次不更新，不能清掉已有兜底值。
                if runtime_image.is_some() {
                    instance.runtime_image = runtime_image;
                }
                if runtime_version.is_some() {
                    instance.runtime_version = runtime_version;
                }
                Ok(instance.clone())
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update hermes_instances
                    set runtime_image = coalesce($2, runtime_image),
                        runtime_version = coalesce($3, runtime_version),
                        updated_at = now()
                    where id = $1::uuid
                    returning id::text as id,
                              user_id::text as user_id,
                              kind,
                              status,
                              name,
                              api_token_secret_ref,
                              container_id,
                              host_workspace_path,
                              host_sandbox_path,
                              host_config_path,
                              health_status,
                              status_message,
                              runtime_image,
                              runtime_version
                    "#,
                )
                .bind(instance_id)
                .bind(runtime_image)
                .bind(runtime_version)
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?
                .ok_or(StoreError::InviteNotFound)?;

                row_to_hermes_instance(&row, cipher)
            }),
        }
    }

    pub async fn record_hermes_scheduler_snapshot(
        &self,
        instance_id: &str,
        input: HermesSchedulerSnapshotInput,
    ) -> Result<HermesSchedulerSnapshot, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let instance = inner
                    .hermes_instances_by_user_id
                    .values()
                    .find(|instance| instance.id == instance_id)
                    .cloned()
                    .ok_or(StoreError::InviteNotFound)?;
                let user_email = inner
                    .users_by_id
                    .get(&instance.user_id)
                    .map(|user| user.email.clone());
                let snapshot = HermesSchedulerSnapshot {
                    user_id: instance.user_id.clone(),
                    user_email,
                    hermes_instance_id: instance.id.clone(),
                    instance_status: hermes_status_as_str(&instance.status).to_string(),
                    scheduler_status: input.scheduler_status,
                    scheduler_enabled: input.scheduler_enabled,
                    running_jobs_count: input.running_jobs_count,
                    reported_at: input.reported_at,
                    source: input.source,
                    snapshot_hash: input.snapshot_hash,
                    next_wake_at: input.next_wake_at,
                    tasks: input.tasks,
                };
                inner
                    .hermes_scheduler_snapshots_by_instance_id
                    .insert(instance.id.clone(), snapshot.clone());
                Ok(snapshot)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let tasks =
                    serde_json::to_value(&input.tasks).map_err(|_| StoreError::DatabaseFailed)?;
                sqlx::query(
                    r#"
                    insert into hermes_scheduler_snapshots (
                        hermes_instance_id, scheduler_status, scheduler_enabled,
                        running_jobs_count, source, snapshot_hash, next_wake_at,
                        tasks, reported_at, updated_at
                    )
                    values (
                        $1::uuid, $2, $3, $4, $5, $6, to_timestamp($7),
                        $8, to_timestamp($9), now()
                    )
                    on conflict (hermes_instance_id) do update
                    set scheduler_status = excluded.scheduler_status,
                        scheduler_enabled = excluded.scheduler_enabled,
                        running_jobs_count = excluded.running_jobs_count,
                        source = excluded.source,
                        snapshot_hash = excluded.snapshot_hash,
                        next_wake_at = excluded.next_wake_at,
                        tasks = excluded.tasks,
                        reported_at = excluded.reported_at,
                        updated_at = now()
                    "#,
                )
                .bind(instance_id)
                .bind(&input.scheduler_status)
                .bind(input.scheduler_enabled)
                .bind(input.running_jobs_count as i32)
                .bind(&input.source)
                .bind(&input.snapshot_hash)
                .bind(input.next_wake_at.map(|value| value as f64))
                .bind(tasks)
                .bind(input.reported_at as f64)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                let row = sqlx::query(hermes_scheduler_snapshot_select(
                    "where hermes_scheduler_snapshots.hermes_instance_id = $1::uuid",
                    "",
                ))
                .bind(instance_id)
                .fetch_one(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;
                row_to_scheduler_snapshot(&row)
            }),
        }
    }

    pub async fn list_hermes_scheduler_snapshots(
        &self,
    ) -> Result<Vec<HermesSchedulerSnapshot>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let mut snapshots = inner
                    .hermes_scheduler_snapshots_by_instance_id
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                snapshots.sort_by(|left, right| {
                    left.user_email
                        .as_deref()
                        .unwrap_or(&left.user_id)
                        .cmp(right.user_email.as_deref().unwrap_or(&right.user_id))
                });
                Ok(snapshots)
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let rows = sqlx::query(hermes_scheduler_snapshot_select(
                    "",
                    "order by users.email asc",
                ))
                .fetch_all(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                rows.iter().map(row_to_scheduler_snapshot).collect()
            }),
        }
    }

    pub async fn hermes_scheduler_snapshot_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Option<HermesSchedulerSnapshot>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                Ok(inner
                    .hermes_scheduler_snapshots_by_instance_id
                    .get(instance_id)
                    .cloned())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let rows = sqlx::query(hermes_scheduler_snapshot_select(
                    "where hermes_scheduler_snapshots.hermes_instance_id = $1::uuid",
                    "",
                ))
                .bind(instance_id)
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                rows.as_ref().map(row_to_scheduler_snapshot).transpose()
            }),
        }
    }

    pub async fn record_hermes_user_activity(&self, user_id: &str) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let instance = inner
                    .hermes_instances_by_user_id
                    .get(user_id)
                    .ok_or(StoreError::InviteNotFound)?
                    .clone();
                let state = inner
                    .hermes_lifecycle_by_instance_id
                    .entry(instance.id.clone())
                    .or_insert_with(|| default_lifecycle_state(&instance));
                state.last_user_activity_at = Some(unix_now());
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query(
                    r#"
                    update hermes_instances
                    set last_user_activity_at = now(),
                        updated_at = now()
                    where user_id = $1::uuid
                    "#,
                )
                .bind(user_id)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;
                Ok(())
            }),
        }
    }

    pub async fn set_hermes_instance_stopped_reason(
        &self,
        instance_id: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let instance = inner
                    .hermes_instances_by_user_id
                    .values()
                    .find(|instance| instance.id == instance_id)
                    .cloned()
                    .ok_or(StoreError::InviteNotFound)?;
                let state = inner
                    .hermes_lifecycle_by_instance_id
                    .entry(instance.id.clone())
                    .or_insert_with(|| default_lifecycle_state(&instance));
                state.last_stopped_at = Some(unix_now());
                state.stopped_reason = Some(reason.to_string());
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query(
                    r#"
                    update hermes_instances
                    set stopped_reason = $2,
                        last_stopped_at = now(),
                        updated_at = now()
                    where id = $1::uuid
                    "#,
                )
                .bind(instance_id)
                .bind(reason)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;
                Ok(())
            }),
        }
    }

    pub async fn list_hermes_lifecycle_candidates(
        &self,
    ) -> Result<Vec<HermesLifecycleCandidate>, StoreError> {
        let instances = self.list_hermes_instances().await?;
        let mut candidates = Vec::with_capacity(instances.len());
        for instance in instances {
            let lifecycle = self
                .hermes_lifecycle_state_for_instance(&instance)
                .await?
                .unwrap_or_else(|| default_lifecycle_state(&instance));
            let scheduler_snapshot = self
                .hermes_scheduler_snapshot_for_instance(&instance.id)
                .await?;
            candidates.push(HermesLifecycleCandidate {
                instance,
                lifecycle,
                scheduler_snapshot,
            });
        }
        Ok(candidates)
    }

    async fn hermes_lifecycle_state_for_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<Option<HermesLifecycleState>, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                Ok(inner
                    .hermes_lifecycle_by_instance_id
                    .get(&instance.id)
                    .cloned())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    select id::text as instance_id,
                           user_id::text as user_id,
                           extract(epoch from last_user_activity_at)::bigint as last_user_activity_at,
                           extract(epoch from last_started_at)::bigint as last_started_at,
                           extract(epoch from last_stopped_at)::bigint as last_stopped_at,
                           stopped_reason
                    from hermes_instances
                    where id = $1::uuid
                    "#,
                )
                .bind(&instance.id)
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                row.as_ref().map(row_to_lifecycle_state).transpose()
            }),
        }
    }

    pub async fn user_by_session_cookie(
        &self,
        cookie: &str,
        cookie_name: &str,
    ) -> Result<User, StoreError> {
        let token = cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find_map(|(name, value)| {
                if name == cookie_name {
                    Some(value)
                } else {
                    None
                }
            })
            .ok_or(StoreError::Unauthorized)?;

        self.user_by_session_token(token).await
    }

    pub async fn record_proxy_audit(&self, event: ProxyAuditEvent) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                inner.proxy_audit_logs.push(event);
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query(
                    r#"
                    insert into proxy_audit_logs (
                        id, user_id, hermes_instance_id, direction, method, path,
                        status_code, duration_ms, error_code
                    )
                    values ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9)
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(optional_uuid(event.user_id.as_deref())?)
                .bind(optional_uuid(event.hermes_instance_id.as_deref())?)
                .bind(event.direction)
                .bind(event.method)
                .bind(event.path)
                .bind(event.status_code.map(i32::from))
                .bind(optional_i32(event.duration_ms)?)
                .bind(event.error_code)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(())
            }),
        }
    }

    pub async fn record_llm_usage(&self, event: LlmUsageEvent) -> Result<(), StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                inner.llm_usage_events.push(event);
                Ok(())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                sqlx::query(
                    r#"
                    insert into llm_usage_events (
                        id, user_id, hermes_instance_id, model, upstream_provider,
                        status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens
                    )
                    values ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    "#,
                )
                .bind(Uuid::new_v4().to_string())
                .bind(optional_uuid(event.user_id.as_deref())?)
                .bind(optional_uuid(event.hermes_instance_id.as_deref())?)
                .bind(event.model)
                .bind(event.upstream_provider)
                .bind(event.status_code.map(i32::from))
                .bind(optional_i32(event.duration_ms)?)
                .bind(optional_u32_as_i32(event.prompt_tokens)?)
                .bind(optional_u32_as_i32(event.completion_tokens)?)
                .bind(optional_u32_as_i32(event.total_tokens)?)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(())
            }),
        }
    }

    pub async fn system_settings(&self) -> Result<SystemSettings, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                Ok(inner.system_settings.clone())
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let value = sqlx::query("select value from system_settings where key = $1")
                    .bind(MAX_SESSIONS_PER_USER_KEY)
                    .fetch_optional(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .and_then(|row| row.try_get::<String, _>("value").ok())
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(DEFAULT_MAX_SESSIONS_PER_USER);

                let oidc = sqlx::query("select value from system_settings where key = $1")
                    .bind(OIDC_SETTINGS_KEY)
                    .fetch_optional(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .and_then(|row| row.try_get::<String, _>("value").ok())
                    .map(|value| {
                        serde_json::from_str::<OidcSettings>(&value)
                            .map_err(|_| StoreError::DatabaseFailed)
                            .and_then(|settings| decrypt_oidc_settings(settings, cipher))
                    })
                    .transpose()?
                    .unwrap_or_default();

                Ok(SystemSettings {
                    max_sessions_per_user: value,
                    oidc,
                })
            }),
        }
    }

    pub async fn update_system_settings(
        &self,
        settings: SystemSettings,
    ) -> Result<SystemSettings, StoreError> {
        validate_system_settings(&settings)?;
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                inner.system_settings = settings.clone();
                Ok(settings)
            }
            SessionStoreBackend::Postgres { pool, cipher } => block_on_db(async {
                let stored_oidc = encrypted_oidc_settings(&settings.oidc, cipher);
                sqlx::query(
                    r#"
                    insert into system_settings (key, value, updated_at)
                    values ($1, $2, now())
                    on conflict (key) do update set
                        value = excluded.value,
                        updated_at = now()
                    "#,
                )
                .bind(MAX_SESSIONS_PER_USER_KEY)
                .bind(settings.max_sessions_per_user.to_string())
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                sqlx::query(
                    r#"
                    insert into system_settings (key, value, updated_at)
                    values ($1, $2, now())
                    on conflict (key) do update set
                        value = excluded.value,
                        updated_at = now()
                    "#,
                )
                .bind(OIDC_SETTINGS_KEY)
                .bind(serde_json::to_string(&stored_oidc).map_err(|_| StoreError::DatabaseFailed)?)
                .execute(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?;

                Ok(settings)
            }),
        }
    }

    pub async fn proxy_audit_count(&self) -> Result<usize, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => inner
                .lock()
                .map(|inner| inner.proxy_audit_logs.len())
                .map_err(|_| StoreError::LockFailed),
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let count = sqlx::query("select count(*)::bigint as count from proxy_audit_logs")
                    .fetch_one(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .try_get::<i64, _>("count")
                    .map_err(|_| StoreError::DatabaseFailed)?;
                usize::try_from(count).map_err(|_| StoreError::DatabaseFailed)
            }),
        }
    }

    pub async fn llm_usage_count(&self) -> Result<usize, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => inner
                .lock()
                .map(|inner| inner.llm_usage_events.len())
                .map_err(|_| StoreError::LockFailed),
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let count = sqlx::query("select count(*)::bigint as count from llm_usage_events")
                    .fetch_one(pool)
                    .await
                    .map_err(|_| StoreError::DatabaseFailed)?
                    .try_get::<i64, _>("count")
                    .map_err(|_| StoreError::DatabaseFailed)?;
                usize::try_from(count).map_err(|_| StoreError::DatabaseFailed)
            }),
        }
    }

    async fn set_user_status(&self, user_id: &str, status: UserStatus) -> Result<User, StoreError> {
        match &self.backend {
            SessionStoreBackend::Memory(inner) => {
                let mut inner = inner.lock().map_err(|_| StoreError::LockFailed)?;
                let user = inner
                    .users_by_id
                    .get_mut(user_id)
                    .ok_or(StoreError::InvalidCredentials)?;
                user.status = status;
                user.updated_at = unix_now();
                Ok(user.clone())
            }
            SessionStoreBackend::Postgres { pool, .. } => block_on_db(async {
                let row = sqlx::query(
                    r#"
                    update users
                    set status = $2, updated_at = now()
                    where id = $1::uuid
                    returning id::text as id,
                              email,
                              password_hash,
                              role,
                              status,
                              extract(epoch from created_at)::bigint as created_at,
                              extract(epoch from updated_at)::bigint as updated_at
                    "#,
                )
                .bind(user_id)
                .bind(user_status_as_str(&status))
                .fetch_optional(pool)
                .await
                .map_err(|_| StoreError::DatabaseFailed)?
                .ok_or(StoreError::InvalidCredentials)?;

                row_to_user(&row)
            }),
        }
    }
}

fn validate_system_settings(settings: &SystemSettings) -> Result<(), StoreError> {
    if settings.max_sessions_per_user == 0
        || settings.max_sessions_per_user > MAX_CONFIGURABLE_SESSIONS_PER_USER
    {
        return Err(StoreError::InvalidSystemSettings);
    }
    if settings.oidc.enabled {
        let oidc = &settings.oidc;
        if oidc.client_id.trim().is_empty()
            || oidc.client_secret.trim().is_empty()
            || oidc.authorization_url.trim().is_empty()
            || oidc.token_url.trim().is_empty()
            || oidc.userinfo_url.trim().is_empty()
            || oidc.email_claim.trim().is_empty()
        {
            return Err(StoreError::InvalidSystemSettings);
        }
    }

    Ok(())
}

fn encrypted_oidc_settings(settings: &OidcSettings, cipher: &SecretCipher) -> OidcSettings {
    let mut stored = settings.clone();
    if !stored.client_secret.is_empty() {
        stored.client_secret = encrypt_secret(cipher, &stored.client_secret);
    }
    stored
}

fn decrypt_oidc_settings(
    mut settings: OidcSettings,
    cipher: &SecretCipher,
) -> Result<OidcSettings, StoreError> {
    if looks_like_encrypted_secret(&settings.client_secret) {
        settings.client_secret = decrypt_secret(cipher, &settings.client_secret)
            .map_err(|_| StoreError::SecretFailed)?;
    }
    Ok(settings)
}

fn looks_like_encrypted_secret(value: &str) -> bool {
    let mut parts = value.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("v1"), Some(_), Some(_), None)
    )
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

fn register_with_invite_in_memory(
    inner: &mut StoreInner,
    invite_token: &str,
    email: &str,
    password: &str,
) -> Result<User, StoreError> {
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

fn login_in_memory(inner: &StoreInner, email: &str, password: &str) -> Result<User, StoreError> {
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

    let verified =
        verify_password(&user.password_hash, password).map_err(|_| StoreError::PasswordFailed)?;

    if !verified {
        return Err(StoreError::InvalidCredentials);
    }

    Ok(user.clone())
}

fn user_by_session_token_in_memory(
    inner: &mut StoreInner,
    token: &str,
) -> Result<User, StoreError> {
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

fn create_invite_in_memory(
    inner: &mut StoreInner,
    created_by_user_id: &str,
    expires_at: u64,
    max_uses: u32,
) -> Result<CreatedInvite, StoreError> {
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

fn list_invites_in_memory(inner: &mut StoreInner) -> Result<Vec<PublicInvite>, StoreError> {
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

fn revoke_invite_in_memory(
    inner: &mut StoreInner,
    invite_id: &str,
) -> Result<PublicInvite, StoreError> {
    let now = unix_now();
    let invite = inner
        .invites_by_id
        .get_mut(invite_id)
        .ok_or(StoreError::InviteIdNotFound)?;

    invite.status = InviteStatus::Revoked;
    invite.updated_at = now;
    Ok(invite.public())
}

async fn postgres_create_user_with_executor<'e, E>(
    executor: E,
    email: &str,
    password: &str,
    role: UserRole,
) -> Result<User, StoreError>
where
    E: Executor<'e, Database = Postgres>,
{
    let email = normalize_email(email);
    let now = unix_now();
    let user = User {
        id: Uuid::new_v4().to_string(),
        email,
        password_hash: hash_password(password).map_err(|_| StoreError::PasswordFailed)?,
        role,
        status: UserStatus::Active,
        created_at: now,
        updated_at: now,
    };

    sqlx::query(
        r#"
        insert into users (id, email, password_hash, role, status, created_at, updated_at)
        values ($1::uuid, $2, $3, $4, $5, to_timestamp($6), to_timestamp($7))
        "#,
    )
    .bind(&user.id)
    .bind(&user.email)
    .bind(&user.password_hash)
    .bind(user_role_as_str(&user.role))
    .bind(user_status_as_str(&user.status))
    .bind(user.created_at as f64)
    .bind(user.updated_at as f64)
    .execute(executor)
    .await
    .map_err(|_| StoreError::DatabaseFailed)?;

    Ok(user)
}

async fn postgres_create_user(
    pool: &PgPool,
    email: &str,
    password: &str,
    role: UserRole,
) -> Result<User, StoreError> {
    postgres_create_user_with_executor(pool, email, password, role).await
}

async fn postgres_user_id_by_email_with_executor<'e, E>(
    executor: E,
    email: &str,
) -> Result<Option<String>, StoreError>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query("select id::text as id from users where email = $1")
        .bind(normalize_email(email))
        .fetch_optional(executor)
        .await
        .map_err(|_| StoreError::DatabaseFailed)?;

    row.map(|row| row.try_get("id").map_err(|_| StoreError::DatabaseFailed))
        .transpose()
}

async fn postgres_user_by_email(pool: &PgPool, email: &str) -> Result<Option<User>, StoreError> {
    let row = sqlx::query(
        r#"
        select id::text as id,
               email,
               password_hash,
               role,
               status,
               extract(epoch from created_at)::bigint as created_at,
               extract(epoch from updated_at)::bigint as updated_at
        from users
        where email = $1
        "#,
    )
    .bind(normalize_email(email))
    .fetch_optional(pool)
    .await
    .map_err(|_| StoreError::DatabaseFailed)?;

    row.map(|row| row_to_user(&row)).transpose()
}

async fn mark_invite_status_with_executor<'e, E>(
    executor: E,
    invite_id: &str,
    status: InviteStatus,
) -> Result<(), StoreError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query("update invites set status = $2, updated_at = now() where id = $1::uuid")
        .bind(invite_id)
        .bind(invite_status_as_str(&status))
        .execute(executor)
        .await
        .map_err(|_| StoreError::DatabaseFailed)?;
    Ok(())
}

fn invite_select(prefix: &str, filter: &str, suffix: &str) -> String {
    format!(
        r#"{prefix}
           id::text as id,
           token_hash,
           coalesce(created_by_user_id::text, '') as created_by_user_id,
           status,
           extract(epoch from expires_at)::bigint as expires_at,
           max_uses,
           used_count,
           extract(epoch from created_at)::bigint as created_at,
           extract(epoch from updated_at)::bigint as updated_at
           from invites
           {filter}
           {suffix}"#
    )
}

fn hermes_instance_select(prefix: &str, filter: &str, suffix: &str) -> String {
    format!(
        r#"{prefix}
           id::text as id,
           user_id::text as user_id,
           kind,
           status,
           name,
           api_token_secret_ref,
           container_id,
           host_workspace_path,
           host_sandbox_path,
           host_config_path,
           health_status,
           status_message,
           runtime_image,
           runtime_version
           from hermes_instances
           {filter}
           {suffix}"#
    )
}

fn hermes_scheduler_snapshot_select(filter: &str, suffix: &str) -> &'static str {
    // 当前只需要两个固定查询形态，返回 &'static str 可以避免动态 SQL 生命周期噪音。
    match (filter.is_empty(), suffix.is_empty()) {
        (true, false) => {
            r#"
            select users.id::text as user_id,
                   users.email as user_email,
                   hermes_instances.id::text as hermes_instance_id,
                   hermes_instances.status as instance_status,
                   hermes_scheduler_snapshots.scheduler_status,
                   hermes_scheduler_snapshots.scheduler_enabled,
                   hermes_scheduler_snapshots.running_jobs_count,
                   hermes_scheduler_snapshots.source,
                   hermes_scheduler_snapshots.snapshot_hash,
                   extract(epoch from hermes_scheduler_snapshots.next_wake_at)::bigint as next_wake_at,
                   hermes_scheduler_snapshots.tasks,
                   extract(epoch from hermes_scheduler_snapshots.reported_at)::bigint as reported_at
            from hermes_scheduler_snapshots
            join hermes_instances on hermes_instances.id = hermes_scheduler_snapshots.hermes_instance_id
            join users on users.id = hermes_instances.user_id
            order by users.email asc
            "#
        }
        (false, true) => {
            r#"
            select users.id::text as user_id,
                   users.email as user_email,
                   hermes_instances.id::text as hermes_instance_id,
                   hermes_instances.status as instance_status,
                   hermes_scheduler_snapshots.scheduler_status,
                   hermes_scheduler_snapshots.scheduler_enabled,
                   hermes_scheduler_snapshots.running_jobs_count,
                   hermes_scheduler_snapshots.source,
                   hermes_scheduler_snapshots.snapshot_hash,
                   extract(epoch from hermes_scheduler_snapshots.next_wake_at)::bigint as next_wake_at,
                   hermes_scheduler_snapshots.tasks,
                   extract(epoch from hermes_scheduler_snapshots.reported_at)::bigint as reported_at
            from hermes_scheduler_snapshots
            join hermes_instances on hermes_instances.id = hermes_scheduler_snapshots.hermes_instance_id
            join users on users.id = hermes_instances.user_id
            where hermes_scheduler_snapshots.hermes_instance_id = $1::uuid
            "#
        }
        _ => unreachable!("unsupported scheduler snapshot query shape"),
    }
}

fn row_to_user(row: &sqlx::postgres::PgRow) -> Result<User, StoreError> {
    let role = row
        .try_get::<String, _>("role")
        .map_err(|_| StoreError::DatabaseFailed)?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(|_| StoreError::DatabaseFailed)?;

    Ok(User {
        id: row.try_get("id").map_err(|_| StoreError::DatabaseFailed)?,
        email: row
            .try_get("email")
            .map_err(|_| StoreError::DatabaseFailed)?,
        password_hash: row
            .try_get("password_hash")
            .map_err(|_| StoreError::DatabaseFailed)?,
        role: parse_user_role(&role)?,
        status: parse_user_status(&status)?,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
        updated_at: row
            .try_get::<i64, _>("updated_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
    })
}

fn row_to_invite(row: &sqlx::postgres::PgRow) -> Result<Invite, StoreError> {
    let status = row
        .try_get::<String, _>("status")
        .map_err(|_| StoreError::DatabaseFailed)?;

    Ok(Invite {
        id: row.try_get("id").map_err(|_| StoreError::DatabaseFailed)?,
        token_hash: row
            .try_get("token_hash")
            .map_err(|_| StoreError::DatabaseFailed)?,
        created_by_user_id: row
            .try_get("created_by_user_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        status: parse_invite_status(&status)?,
        expires_at: row
            .try_get::<i64, _>("expires_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
        max_uses: row
            .try_get::<i32, _>("max_uses")
            .map_err(|_| StoreError::DatabaseFailed)? as u32,
        used_count: row
            .try_get::<i32, _>("used_count")
            .map_err(|_| StoreError::DatabaseFailed)? as u32,
        created_at: row
            .try_get::<i64, _>("created_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
        updated_at: row
            .try_get::<i64, _>("updated_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
    })
}

fn row_to_hermes_instance(
    row: &sqlx::postgres::PgRow,
    cipher: &SecretCipher,
) -> Result<HermesInstance, StoreError> {
    let kind = row
        .try_get::<String, _>("kind")
        .map_err(|_| StoreError::DatabaseFailed)?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(|_| StoreError::DatabaseFailed)?;
    let encrypted_token = row
        .try_get::<Option<String>, _>("api_token_secret_ref")
        .map_err(|_| StoreError::DatabaseFailed)?;
    let api_token_secret_ref = encrypted_token
        .as_deref()
        .map(|token| decrypt_secret(cipher, token).map_err(|_| StoreError::SecretFailed))
        .transpose()?;

    Ok(HermesInstance {
        id: row.try_get("id").map_err(|_| StoreError::DatabaseFailed)?,
        user_id: row
            .try_get("user_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        kind: parse_hermes_kind(&kind)?,
        status: parse_hermes_status(&status)?,
        name: row
            .try_get("name")
            .map_err(|_| StoreError::DatabaseFailed)?,
        api_token_secret_ref,
        llm_api_key: None,
        container_id: row
            .try_get("container_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        host_workspace_path: row
            .try_get("host_workspace_path")
            .map_err(|_| StoreError::DatabaseFailed)?,
        host_sandbox_path: row
            .try_get("host_sandbox_path")
            .map_err(|_| StoreError::DatabaseFailed)?,
        host_config_path: row
            .try_get("host_config_path")
            .map_err(|_| StoreError::DatabaseFailed)?,
        health_status: row
            .try_get("health_status")
            .map_err(|_| StoreError::DatabaseFailed)?,
        status_message: row
            .try_get("status_message")
            .map_err(|_| StoreError::DatabaseFailed)?,
        runtime_image: row
            .try_get("runtime_image")
            .map_err(|_| StoreError::DatabaseFailed)?,
        runtime_version: row
            .try_get("runtime_version")
            .map_err(|_| StoreError::DatabaseFailed)?,
        global_skills_write_enabled: false,
    })
}

fn row_to_scheduler_snapshot(
    row: &sqlx::postgres::PgRow,
) -> Result<HermesSchedulerSnapshot, StoreError> {
    let tasks_value = row
        .try_get::<serde_json::Value, _>("tasks")
        .map_err(|_| StoreError::DatabaseFailed)?;
    let tasks = serde_json::from_value::<Vec<HermesScheduledTaskSnapshot>>(tasks_value)
        .map_err(|_| StoreError::DatabaseFailed)?;

    Ok(HermesSchedulerSnapshot {
        user_id: row
            .try_get("user_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        user_email: row
            .try_get("user_email")
            .map_err(|_| StoreError::DatabaseFailed)?,
        hermes_instance_id: row
            .try_get("hermes_instance_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        instance_status: row
            .try_get("instance_status")
            .map_err(|_| StoreError::DatabaseFailed)?,
        scheduler_status: row
            .try_get("scheduler_status")
            .map_err(|_| StoreError::DatabaseFailed)?,
        scheduler_enabled: row
            .try_get("scheduler_enabled")
            .map_err(|_| StoreError::DatabaseFailed)?,
        running_jobs_count: row
            .try_get::<i32, _>("running_jobs_count")
            .map_err(|_| StoreError::DatabaseFailed)? as u32,
        reported_at: row
            .try_get::<i64, _>("reported_at")
            .map_err(|_| StoreError::DatabaseFailed)? as u64,
        source: row
            .try_get("source")
            .map_err(|_| StoreError::DatabaseFailed)?,
        snapshot_hash: row
            .try_get("snapshot_hash")
            .map_err(|_| StoreError::DatabaseFailed)?,
        next_wake_at: row
            .try_get::<Option<i64>, _>("next_wake_at")
            .map_err(|_| StoreError::DatabaseFailed)?
            .map(|value| value as u64),
        tasks,
    })
}

fn row_to_lifecycle_state(row: &sqlx::postgres::PgRow) -> Result<HermesLifecycleState, StoreError> {
    Ok(HermesLifecycleState {
        instance_id: row
            .try_get("instance_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        user_id: row
            .try_get("user_id")
            .map_err(|_| StoreError::DatabaseFailed)?,
        last_user_activity_at: row
            .try_get::<Option<i64>, _>("last_user_activity_at")
            .map_err(|_| StoreError::DatabaseFailed)?
            .map(|value| value as u64),
        last_started_at: row
            .try_get::<Option<i64>, _>("last_started_at")
            .map_err(|_| StoreError::DatabaseFailed)?
            .map(|value| value as u64),
        last_stopped_at: row
            .try_get::<Option<i64>, _>("last_stopped_at")
            .map_err(|_| StoreError::DatabaseFailed)?
            .map(|value| value as u64),
        stopped_reason: row
            .try_get("stopped_reason")
            .map_err(|_| StoreError::DatabaseFailed)?,
    })
}

fn default_lifecycle_state(instance: &HermesInstance) -> HermesLifecycleState {
    let now = unix_now();
    HermesLifecycleState {
        instance_id: instance.id.clone(),
        user_id: instance.user_id.clone(),
        last_user_activity_at: Some(now),
        last_started_at: matches!(&instance.status, HermesInstanceStatus::Running).then_some(now),
        last_stopped_at: matches!(&instance.status, HermesInstanceStatus::Stopped).then_some(now),
        stopped_reason: None,
    }
}

fn update_memory_lifecycle_from_instance(
    inner: &mut StoreInner,
    instance: &HermesInstance,
    stopped_reason: Option<&str>,
) {
    let now = unix_now();
    let previous_status = inner
        .hermes_instances_by_user_id
        .get(&instance.user_id)
        .map(|previous| previous.status.clone());
    let state = inner
        .hermes_lifecycle_by_instance_id
        .entry(instance.id.clone())
        .or_insert_with(|| default_lifecycle_state(instance));
    match &instance.status {
        HermesInstanceStatus::Running => {
            if previous_status.as_ref() != Some(&HermesInstanceStatus::Running) {
                state.last_started_at = Some(now);
            }
            state.stopped_reason = None;
        }
        HermesInstanceStatus::Stopped => {
            if previous_status.as_ref() != Some(&HermesInstanceStatus::Stopped) {
                state.last_stopped_at = Some(now);
                state.stopped_reason = Some(stopped_reason.unwrap_or("manual").to_string());
            }
        }
        HermesInstanceStatus::Provisioning | HermesInstanceStatus::Error => {}
    }
}

fn optional_uuid(value: Option<&str>) -> Result<Option<Uuid>, StoreError> {
    value
        .map(|value| Uuid::parse_str(value).map_err(|_| StoreError::DatabaseFailed))
        .transpose()
}

fn optional_i32(value: Option<u64>) -> Result<Option<i32>, StoreError> {
    value
        .map(|value| i32::try_from(value).map_err(|_| StoreError::DatabaseFailed))
        .transpose()
}

fn optional_u32_as_i32(value: Option<u32>) -> Result<Option<i32>, StoreError> {
    value
        .map(|value| i32::try_from(value).map_err(|_| StoreError::DatabaseFailed))
        .transpose()
}

fn parse_user_role(value: &str) -> Result<UserRole, StoreError> {
    match value {
        "admin" => Ok(UserRole::Admin),
        "user" => Ok(UserRole::User),
        _ => Err(StoreError::DatabaseFailed),
    }
}

fn parse_user_status(value: &str) -> Result<UserStatus, StoreError> {
    match value {
        "active" => Ok(UserStatus::Active),
        "disabled" => Ok(UserStatus::Disabled),
        _ => Err(StoreError::DatabaseFailed),
    }
}

fn parse_invite_status(value: &str) -> Result<InviteStatus, StoreError> {
    match value {
        "pending" => Ok(InviteStatus::Pending),
        "revoked" => Ok(InviteStatus::Revoked),
        "expired" => Ok(InviteStatus::Expired),
        "exhausted" => Ok(InviteStatus::Exhausted),
        _ => Err(StoreError::DatabaseFailed),
    }
}

fn parse_hermes_kind(value: &str) -> Result<HermesInstanceKind, StoreError> {
    match value {
        "managed_docker" => Ok(HermesInstanceKind::ManagedDocker),
        _ => Err(StoreError::DatabaseFailed),
    }
}

fn parse_hermes_status(value: &str) -> Result<HermesInstanceStatus, StoreError> {
    match value {
        "provisioning" => Ok(HermesInstanceStatus::Provisioning),
        "running" => Ok(HermesInstanceStatus::Running),
        "stopped" => Ok(HermesInstanceStatus::Stopped),
        "error" => Ok(HermesInstanceStatus::Error),
        _ => Err(StoreError::DatabaseFailed),
    }
}

fn user_role_as_str(role: &UserRole) -> &'static str {
    match role {
        UserRole::Admin => "admin",
        UserRole::User => "user",
    }
}

fn user_status_as_str(status: &UserStatus) -> &'static str {
    match status {
        UserStatus::Active => "active",
        UserStatus::Disabled => "disabled",
    }
}

fn invite_status_as_str(status: &InviteStatus) -> &'static str {
    match status {
        InviteStatus::Pending => "pending",
        InviteStatus::Revoked => "revoked",
        InviteStatus::Expired => "expired",
        InviteStatus::Exhausted => "exhausted",
    }
}

fn hermes_kind_as_str(kind: &HermesInstanceKind) -> &'static str {
    match kind {
        HermesInstanceKind::ManagedDocker => "managed_docker",
    }
}

fn hermes_status_as_str(status: &HermesInstanceStatus) -> &'static str {
    match status {
        HermesInstanceStatus::Provisioning => "provisioning",
        HermesInstanceStatus::Running => "running",
        HermesInstanceStatus::Stopped => "stopped",
        HermesInstanceStatus::Error => "error",
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
