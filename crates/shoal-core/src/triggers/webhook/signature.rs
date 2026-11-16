//! A webhook request's signature: the HMAC-SHA256 of its body under the
//! trigger's secret, in hex, in the header its scheme names.
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use shoal_protocol::triggers::WebhookRequest;

use super::Signature;

impl Signature {
    /// The header, lowercased, that carries it.
    pub fn header(&self) -> &str {
        match self {
            Self::Forgejo => "x-forgejo-signature",
            Self::Github => "x-hub-signature-256",
            Self::Header(header) => header,
        }
    }

    /// Whether `request`, whose body was `body` as sent, carries the
    /// signature of `body` under `secret`.
    pub fn verifies(&self, secret: &[u8], request: &WebhookRequest, body: &[u8]) -> bool {
        let Some(value) = request.header(self.header()) else {
            return false;
        };
        let hex = match self {
            Self::Github => value.strip_prefix("sha256="),
            Self::Header(_) => Some(value.strip_prefix("sha256=").unwrap_or(value)),
            Self::Forgejo => Some(value),
        };
        let Some(signature) = hex.and_then(decode_hex) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
            return false;
        };
        mac.update(body);
        // Compares in constant time, so the time taken tells nothing of
        // the expected signature.
        mac.verify_slice(&signature).is_ok()
    }
}

/// The bytes `text` spells in hex, either case; none for anything else.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests;
