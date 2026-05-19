use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use thiserror::Error;

/// 应用层密钥封装。
///
/// 真实密钥只从环境变量进入 Hub，落库时只保存加密后的密文。
#[derive(Clone)]
pub struct SecretCipher {
    key: [u8; 32],
}

#[derive(Debug, Error)]
pub enum SecretCipherError {
    #[error("invalid master key")]
    InvalidMasterKey,
    #[error("invalid secret payload")]
    InvalidSecretPayload,
    #[error("decrypt failed")]
    DecryptFailed,
}

impl SecretCipher {
    /// 从 base64 编码的 32 字节主密钥构建 cipher。
    pub fn from_master_key(master_key_b64: &str) -> Result<Self, SecretCipherError> {
        let decoded = STANDARD_NO_PAD
            .decode(master_key_b64)
            .map_err(|_| SecretCipherError::InvalidMasterKey)?;

        let key: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| SecretCipherError::InvalidMasterKey)?;

        Ok(Self { key })
    }
}

/// 加密一个明文 secret，返回可安全落库的字符串。
pub fn encrypt_secret(cipher: &SecretCipher, plaintext: &str) -> String {
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).expect("os rng must be available");

    let aead = XChaCha20Poly1305::new((&cipher.key).into());
    let ciphertext = aead
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_bytes())
        .expect("encryption should not fail");

    format!(
        "v1.{}.{}",
        STANDARD_NO_PAD.encode(nonce),
        STANDARD_NO_PAD.encode(ciphertext)
    )
}

/// 解密落库的 secret，供运行时读取 provider key、instance token 等信息。
pub fn decrypt_secret(cipher: &SecretCipher, payload: &str) -> Result<String, SecretCipherError> {
    let mut parts = payload.splitn(3, '.');
    let version = parts
        .next()
        .ok_or(SecretCipherError::InvalidSecretPayload)?;
    let nonce_b64 = parts
        .next()
        .ok_or(SecretCipherError::InvalidSecretPayload)?;
    let ciphertext_b64 = parts
        .next()
        .ok_or(SecretCipherError::InvalidSecretPayload)?;

    if version != "v1" {
        return Err(SecretCipherError::InvalidSecretPayload);
    }

    let nonce = STANDARD_NO_PAD
        .decode(nonce_b64)
        .map_err(|_| SecretCipherError::InvalidSecretPayload)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(ciphertext_b64)
        .map_err(|_| SecretCipherError::InvalidSecretPayload)?;

    let nonce: [u8; 24] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| SecretCipherError::InvalidSecretPayload)?;

    let aead = XChaCha20Poly1305::new((&cipher.key).into());
    let plaintext = aead
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| SecretCipherError::DecryptFailed)?;

    String::from_utf8(plaintext).map_err(|_| SecretCipherError::DecryptFailed)
}
