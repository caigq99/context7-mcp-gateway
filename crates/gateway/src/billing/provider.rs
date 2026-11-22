use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use uuid::Uuid;
use zeroize::Zeroizing;

pub(super) const PROVIDER: &str = "sandbox";
pub(super) const MERCHANT: &str = "local-sandbox";
pub(super) const CURRENCY: &str = "TST";
const SIGNING_CONTEXT: &[u8] = b"context7-gateway.billing-sandbox.v1\0";

pub(super) struct Sandbox(Zeroizing<[u8; 32]>);

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderEvent {
    pub event_id: Uuid,
    pub order_id: Uuid,
    pub merchant_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: ProviderStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProviderStatus {
    Paid,
    Failed,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum VerificationError {
    #[error("invalid provider signature")]
    Signature,
    #[error("invalid provider event")]
    Event,
}

/// Server-side payment event boundary. Only a provider implementation can verify an external event.
pub(super) trait PaymentProvider {
    fn verify(&self, signature: &str, body: &[u8]) -> Result<ProviderEvent, VerificationError>;
}

impl Sandbox {
    pub fn new(secret: Zeroizing<[u8; 32]>) -> Self {
        Self(secret)
    }

    fn mac(&self, body: &[u8]) -> Result<HmacSha256, VerificationError> {
        let mut mac = HmacSha256::new_from_slice(self.0.as_ref())
            .map_err(|_| VerificationError::Signature)?;
        mac.update(SIGNING_CONTEXT);
        mac.update(body);
        Ok(mac)
    }

    #[cfg(test)]
    pub fn sign(&self, body: &[u8]) -> Result<String, VerificationError> {
        self.mac(body)
            .map(|mac| URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

impl PaymentProvider for Sandbox {
    fn verify(&self, signature: &str, body: &[u8]) -> Result<ProviderEvent, VerificationError> {
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| VerificationError::Signature)?;
        self.mac(body)?
            .verify_slice(&signature)
            .map_err(|_| VerificationError::Signature)?;
        serde_json::from_slice(body).map_err(|_| VerificationError::Event)
    }
}
