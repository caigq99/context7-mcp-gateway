use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PoolId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UpstreamKeyId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GatewayTokenId(pub String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("credential encryption is not configured")]
    EncryptionUnavailable,
    #[error("credential encryption or decryption failed")]
    CryptographicFailure,
    #[error("upstream API key is empty or contains control characters")]
    InvalidUpstreamKey,
    #[error("gateway token generation failed")]
    RandomnessUnavailable,
    #[error("gateway token format is invalid")]
    InvalidGatewayToken,
}

#[derive(Clone, Copy)]
pub struct CredentialCipher {
    key: [u8; 32],
}

impl std::fmt::Debug for CredentialCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialCipher([REDACTED])")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct EncryptedCredential {
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

#[derive(PartialEq, Eq)]
pub struct IssuedGatewayToken {
    pub id: GatewayTokenId,
    pub raw_token: String,
    pub prefix: String,
    pub digest: [u8; 32],
}

impl CredentialCipher {
    pub fn new(key: Option<[u8; 32]>) -> Result<Self, CredentialError> {
        key.map(|key| Self { key })
            .ok_or(CredentialError::EncryptionUnavailable)
    }

    pub fn encrypt(
        &self,
        key_id: &UpstreamKeyId,
        pool_id: &PoolId,
        api_key: &str,
    ) -> Result<EncryptedCredential, CredentialError> {
        if api_key.trim().is_empty() || api_key.chars().any(char::is_control) {
            return Err(CredentialError::InvalidUpstreamKey);
        }
        self.seal(&associated_data(key_id, pool_id), api_key.as_bytes())
    }

    pub fn decrypt(
        &self,
        key_id: &UpstreamKeyId,
        pool_id: &PoolId,
        encrypted: &EncryptedCredential,
    ) -> Result<String, CredentialError> {
        let plaintext = self.open(&associated_data(key_id, pool_id), encrypted)?;
        String::from_utf8(plaintext).map_err(|_| CredentialError::CryptographicFailure)
    }

    pub fn encrypt_gateway_token(
        &self,
        token_id: &GatewayTokenId,
        token: &str,
    ) -> Result<EncryptedCredential, CredentialError> {
        if token.is_empty() {
            return Err(CredentialError::InvalidGatewayToken);
        }
        self.seal(&gateway_token_associated_data(token_id), token.as_bytes())
    }

    pub fn decrypt_gateway_token(
        &self,
        token_id: &GatewayTokenId,
        encrypted: &EncryptedCredential,
    ) -> Result<String, CredentialError> {
        let plaintext = self.open(&gateway_token_associated_data(token_id), encrypted)?;
        String::from_utf8(plaintext).map_err(|_| CredentialError::CryptographicFailure)
    }

    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<EncryptedCredential, CredentialError> {
        let cipher = ChaCha20Poly1305::new((&self.key).into());
        let mut nonce_bytes = [0_u8; 12];
        getrandom::fill(&mut nonce_bytes).map_err(|_| CredentialError::RandomnessUnavailable)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CredentialError::CryptographicFailure)?;
        Ok(EncryptedCredential {
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    fn open(
        &self,
        aad: &[u8],
        encrypted: &EncryptedCredential,
    ) -> Result<Vec<u8>, CredentialError> {
        let cipher = ChaCha20Poly1305::new((&self.key).into());
        cipher
            .decrypt(
                Nonce::from_slice(&encrypted.nonce),
                Payload {
                    msg: &encrypted.ciphertext,
                    aad,
                },
            )
            .map_err(|_| CredentialError::CryptographicFailure)
    }
}

fn associated_data(key_id: &UpstreamKeyId, pool_id: &PoolId) -> Vec<u8> {
    format!(
        "context7-gateway/upstream-key/v1/{}/{}",
        pool_id.0, key_id.0
    )
    .into_bytes()
}

fn gateway_token_associated_data(token_id: &GatewayTokenId) -> Vec<u8> {
    format!("context7-gateway/gateway-token/v1/{}", token_id.0).into_bytes()
}

pub fn issue_gateway_token(
    pepper: Option<[u8; 32]>,
) -> Result<IssuedGatewayToken, CredentialError> {
    let pepper = pepper.ok_or(CredentialError::EncryptionUnavailable)?;
    let id = Uuid::new_v4().simple().to_string();
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|_| CredentialError::RandomnessUnavailable)?;
    let secret = URL_SAFE_NO_PAD.encode(secret);
    let raw_token = format!("c7g_{id}.{secret}");
    let prefix = format!("c7g_{id}");
    let digest = gateway_token_digest(&raw_token, &pepper)?;
    Ok(IssuedGatewayToken {
        id: GatewayTokenId(id),
        raw_token,
        prefix,
        digest,
    })
}

pub fn verify_gateway_token(
    raw_token: &str,
    expected_digest: &[u8],
    pepper: Option<[u8; 32]>,
) -> Result<bool, CredentialError> {
    let pepper = pepper.ok_or(CredentialError::EncryptionUnavailable)?;
    parse_gateway_token(raw_token)?;
    let calculated = gateway_token_digest(raw_token, &pepper)?;
    Ok(calculated.as_slice().ct_eq(expected_digest).into())
}

pub fn parse_gateway_token(raw_token: &str) -> Result<GatewayTokenId, CredentialError> {
    let (prefix, rest) = raw_token
        .split_once('_')
        .ok_or(CredentialError::InvalidGatewayToken)?;
    let (id, secret) = rest
        .split_once('.')
        .ok_or(CredentialError::InvalidGatewayToken)?;
    if prefix != "c7g"
        || id.len() != 32
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || secret.contains('.')
        || secret.len() < 40
        || secret.len() > 50
    {
        return Err(CredentialError::InvalidGatewayToken);
    }
    Ok(GatewayTokenId(id.to_ascii_lowercase()))
}

fn gateway_token_digest(raw_token: &str, pepper: &[u8; 32]) -> Result<[u8; 32], CredentialError> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(pepper)
        .map_err(|_| CredentialError::CryptographicFailure)?;
    mac.update(b"context7-gateway/token/v1\0");
    mac.update(raw_token.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        CredentialCipher, CredentialError, GatewayTokenId, PoolId, UpstreamKeyId,
        issue_gateway_token, parse_gateway_token, verify_gateway_token,
    };

    #[test]
    fn upstream_key_round_trips_and_is_bound_to_its_pool_and_id() {
        let cipher = CredentialCipher::new(Some([7; 32])).expect("cipher key is configured");
        let key_id = UpstreamKeyId("key-a".to_owned());
        let pool_id = PoolId("pool-a".to_owned());
        let encrypted = cipher
            .encrypt(&key_id, &pool_id, "ctx7-secret-key")
            .expect("encryption succeeds");
        assert_eq!(
            cipher.decrypt(&key_id, &pool_id, &encrypted),
            Ok("ctx7-secret-key".to_owned())
        );
        assert_eq!(
            cipher.decrypt(&UpstreamKeyId("key-b".to_owned()), &pool_id, &encrypted),
            Err(CredentialError::CryptographicFailure)
        );
    }

    #[test]
    fn rejects_empty_or_header_injection_api_keys() {
        let cipher = CredentialCipher::new(Some([3; 32])).expect("cipher key is configured");
        let key_id = UpstreamKeyId("key-a".to_owned());
        let pool_id = PoolId("pool-a".to_owned());
        assert_eq!(
            cipher.encrypt(&key_id, &pool_id, "  "),
            Err(CredentialError::InvalidUpstreamKey)
        );
        assert_eq!(
            cipher.encrypt(&key_id, &pool_id, "abc\r\nAuthorization: x"),
            Err(CredentialError::InvalidUpstreamKey)
        );
    }

    #[test]
    fn gateway_token_ciphertext_round_trips_and_is_bound_to_its_id() {
        let cipher = CredentialCipher::new(Some([9; 32])).expect("cipher key is configured");
        let token_id = GatewayTokenId("token-a".to_owned());
        let encrypted = cipher
            .encrypt_gateway_token(&token_id, "c7g_token-a.secret")
            .expect("gateway token encrypts");
        assert_eq!(
            cipher.decrypt_gateway_token(&token_id, &encrypted),
            Ok("c7g_token-a.secret".to_owned())
        );
        assert_eq!(
            cipher.decrypt_gateway_token(&GatewayTokenId("token-b".to_owned()), &encrypted),
            Err(CredentialError::CryptographicFailure)
        );
    }

    #[test]
    fn gateway_tokens_are_parseable_verifiable_and_one_time_random() {
        let pepper = Some([9; 32]);
        let issued = issue_gateway_token(pepper).expect("token creation succeeds");
        assert_eq!(
            parse_gateway_token(&issued.raw_token),
            Ok(issued.id.clone())
        );
        assert!(verify_gateway_token(&issued.raw_token, &issued.digest, pepper).expect("verifies"));
        let other_token = issue_gateway_token(pepper).expect("second token creation succeeds");
        assert!(
            !verify_gateway_token(&other_token.raw_token, &issued.digest, pepper)
                .expect("comparison succeeds")
        );
        assert_ne!(issued.raw_token, other_token.raw_token);
    }

    #[test]
    fn token_parser_rejects_wrong_prefix_and_short_secret() {
        assert_eq!(
            parse_gateway_token("other_a_b"),
            Err(CredentialError::InvalidGatewayToken)
        );
        assert_eq!(
            parse_gateway_token("c7g_0123456789abcdef0123456789abcdef_short"),
            Err(CredentialError::InvalidGatewayToken)
        );
    }
}
