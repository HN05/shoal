use serde::{Deserialize, Serialize};

/// Requests to fetch the secrets a repository configures.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum SecretMethod {
    /// Confirm the caller may fetch a configured secret, requesting approval
    /// when it is gated; the daemon never sees the value.
    SecretAcquire {
        workspace: String,
        name: String,
        request: crate::daemon::secrets::SecretRequest,
    },
    SecretRelease {
        workspace: String,
        name: String,
    },
    SecretCatalog {
        workspace: String,
    },
}
