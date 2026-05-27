use crate::session::store::LdapSettings;
use async_trait::async_trait;
use ldap3::{LdapConnAsync, Scope, SearchEntry};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub type DynLdapAuthenticator = Arc<dyn LdapAuthenticator>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LdapIdentity {
    pub dn: String,
    pub email: String,
}

#[derive(Debug, Error)]
pub enum LdapAuthError {
    #[error("invalid ldap credentials")]
    InvalidCredentials,
    #[error("ldap settings are invalid")]
    Misconfigured,
    #[error("ldap request failed")]
    BackendFailed,
}

#[async_trait]
pub trait LdapAuthenticator: Send + Sync {
    async fn authenticate(
        &self,
        settings: &LdapSettings,
        email: &str,
        password: &str,
    ) -> Result<LdapIdentity, LdapAuthError>;
}

#[derive(Clone, Default)]
pub struct DefaultLdapAuthenticator;

impl DefaultLdapAuthenticator {
    pub fn shared(self) -> DynLdapAuthenticator {
        Arc::new(self)
    }
}

#[async_trait]
impl LdapAuthenticator for DefaultLdapAuthenticator {
    async fn authenticate(
        &self,
        settings: &LdapSettings,
        email: &str,
        password: &str,
    ) -> Result<LdapIdentity, LdapAuthError> {
        let email = normalize_email(email).ok_or(LdapAuthError::InvalidCredentials)?;
        if password.is_empty()
            || settings.url.trim().is_empty()
            || settings.bind_dn.trim().is_empty()
            || settings.base_dn.trim().is_empty()
            || settings.user_filter.trim().is_empty()
            || settings.email_attribute.trim().is_empty()
        {
            return Err(LdapAuthError::Misconfigured);
        }

        let (conn, mut ldap) = LdapConnAsync::new(settings.url.trim())
            .await
            .map_err(|_| LdapAuthError::BackendFailed)?;
        ldap3::drive!(conn);

        ldap.simple_bind(settings.bind_dn.trim(), &settings.bind_password)
            .await
            .map_err(|_| LdapAuthError::BackendFailed)?
            .success()
            .map_err(|_| LdapAuthError::BackendFailed)?;

        let filter = settings
            .user_filter
            .replace("{email}", &escape_ldap_filter_value(&email));
        let attributes = vec![settings.email_attribute.trim()];
        let (entries, _result) = ldap
            .search(settings.base_dn.trim(), Scope::Subtree, &filter, attributes)
            .await
            .map_err(|_| LdapAuthError::BackendFailed)?
            .success()
            .map_err(|_| LdapAuthError::BackendFailed)?;

        let mut entries = entries.into_iter();
        let Some(entry) = entries.next() else {
            let _ = ldap.unbind().await;
            return Err(LdapAuthError::InvalidCredentials);
        };
        if entries.next().is_some() {
            let _ = ldap.unbind().await;
            return Err(LdapAuthError::InvalidCredentials);
        }

        let entry = SearchEntry::construct(entry);
        let returned_email = entry
            .attrs
            .get(settings.email_attribute.trim())
            .and_then(|values| values.first())
            .and_then(|value| normalize_email(value))
            .ok_or(LdapAuthError::InvalidCredentials)?;
        if returned_email != email {
            let _ = ldap.unbind().await;
            return Err(LdapAuthError::InvalidCredentials);
        }

        // 服务账号只负责查找 DN；真正的密码校验必须用用户自己的 DN 再 bind 一次。
        let bind_result = ldap.simple_bind(&entry.dn, password).await;
        let _ = ldap.unbind().await;
        bind_result
            .map_err(|_| LdapAuthError::InvalidCredentials)?
            .success()
            .map_err(|_| LdapAuthError::InvalidCredentials)?;

        Ok(LdapIdentity {
            dn: entry.dn,
            email: returned_email,
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryLdapAuthenticator {
    users_by_email: Arc<Mutex<HashMap<String, InMemoryLdapUser>>>,
}

#[derive(Clone, Debug)]
struct InMemoryLdapUser {
    dn: String,
    email: String,
    password: String,
}

impl InMemoryLdapAuthenticator {
    pub fn shared(self) -> DynLdapAuthenticator {
        Arc::new(self)
    }

    pub fn add_user(&self, dn: &str, email: &str, password: &str) {
        if let Some(normalized_email) = normalize_email(email) {
            let user = InMemoryLdapUser {
                dn: dn.to_string(),
                email: normalized_email.clone(),
                password: password.to_string(),
            };
            self.users_by_email
                .lock()
                .expect("LDAP test users lock")
                .insert(normalized_email, user);
        }
    }
}

#[async_trait]
impl LdapAuthenticator for InMemoryLdapAuthenticator {
    async fn authenticate(
        &self,
        _settings: &LdapSettings,
        email: &str,
        password: &str,
    ) -> Result<LdapIdentity, LdapAuthError> {
        let email = normalize_email(email).ok_or(LdapAuthError::InvalidCredentials)?;
        let users = self
            .users_by_email
            .lock()
            .map_err(|_| LdapAuthError::BackendFailed)?;
        let user = users.get(&email).ok_or(LdapAuthError::InvalidCredentials)?;
        if user.password != password {
            return Err(LdapAuthError::InvalidCredentials);
        }
        Ok(LdapIdentity {
            dn: user.dn.clone(),
            email: user.email.clone(),
        })
    }
}

fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    (!email.is_empty()).then_some(email)
}

/// LDAP 过滤器值的转义规则来自 RFC 4515，避免邮箱中的特殊字符改写查询条件。
fn escape_ldap_filter_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '*' => escaped.push_str("\\2a"),
            '(' => escaped.push_str("\\28"),
            ')' => escaped.push_str("\\29"),
            '\\' => escaped.push_str("\\5c"),
            '\0' => escaped.push_str("\\00"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_ldap_filter_value;

    #[test]
    fn ldap_filter_value_escapes_special_characters() {
        assert_eq!(escape_ldap_filter_value(r#"a*(b)\c"#), r#"a\2a\28b\29\5cc"#);
    }
}
