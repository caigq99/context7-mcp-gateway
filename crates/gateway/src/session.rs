use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("session token pepper is not configured")]
    PepperUnavailable,
    #[error("session token digest failed")]
    DigestFailure,
}

pub fn token_digest(token: &str, pepper: Option<[u8; 32]>) -> Result<[u8; 32], SessionError> {
    mac_for(b"context7-gateway/session/v1\0", token, pepper)
}

pub(crate) fn oauth_verifier(
    state: &str,
    pepper: Option<[u8; 32]>,
) -> Result<[u8; 32], SessionError> {
    mac_for(b"context7-gateway/oauth-pkce/v1\0", state, pepper)
}

pub(crate) fn upstream_fingerprint(
    key: &str,
    pepper: Option<[u8; 32]>,
) -> Result<[u8; 32], SessionError> {
    mac_for(b"context7-gateway/upstream-fingerprint/v1\0", key, pepper)
}

fn mac_for(domain: &[u8], token: &str, pepper: Option<[u8; 32]>) -> Result<[u8; 32], SessionError> {
    let pepper = pepper.ok_or(SessionError::PepperUnavailable)?;
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(&pepper).map_err(|_| SessionError::DigestFailure)?;
    mac.update(domain);
    mac.update(token.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::token_digest;

    #[test]
    fn session_digest_is_stable_and_domain_separated() {
        let first = token_digest("session-secret", Some([4; 32])).expect("digest succeeds");
        let same = token_digest("session-secret", Some([4; 32])).expect("digest succeeds");
        let different = token_digest("session-secret", Some([5; 32])).expect("digest succeeds");
        assert_eq!(first, same);
        assert_ne!(first, different);
    }
}
