use serde::Serialize;

/// Invite lifecycle status. Expired and exhausted states are materialized when
/// an invite is checked or redeemed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InviteStatus {
    Pending,
    Revoked,
    Expired,
    Exhausted,
}

/// Internal invite record. Invite tokens are never stored in plaintext.
#[derive(Clone, Debug)]
pub struct Invite {
    pub id: String,
    pub token_hash: String,
    pub created_by_user_id: String,
    pub status: InviteStatus,
    pub expires_at: u64,
    pub max_uses: u32,
    pub used_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Public invite payload returned to administrators.
#[derive(Clone, Debug, Serialize)]
pub struct PublicInvite {
    pub id: String,
    pub created_by_user_id: String,
    pub status: InviteStatus,
    pub expires_at: u64,
    pub max_uses: u32,
    pub used_count: u32,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Invite {
    pub fn public(&self) -> PublicInvite {
        PublicInvite {
            id: self.id.clone(),
            created_by_user_id: self.created_by_user_id.clone(),
            status: self.status.clone(),
            expires_at: self.expires_at,
            max_uses: self.max_uses,
            used_count: self.used_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
