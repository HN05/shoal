//! The handshake: whether this daemon serves the client's major version,
//! and as whom the connection calls.
use shoal_client::{
    ApiError, Call, ErrorCode,
    wire::{self, HandshakeReply, Hello, Welcome},
};

use super::{Session, errors};
use crate::daemon::Server;

/// The major versions of the API this daemon serves.
const SUPPORTED_MAJORS: &[u64] = &[1];

/// The session `hello` opens, or the refusal to send back.
pub(super) async fn accept(server: &Server, hello: serde_json::Value) -> Result<Session, ApiError> {
    let hello: Hello = serde_json::from_value(hello).map_err(errors::invalid)?;
    let major = wire::major(&hello.version)
        .ok_or_else(|| errors::invalid(format!("unreadable API version {}", hello.version)))?;
    if !SUPPORTED_MAJORS.contains(&major) {
        return Err(ApiError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "this daemon serves API major versions {SUPPORTED_MAJORS:?}, not {}",
                hello.version
            ),
        ));
    }
    let caller = match &hello.scope {
        Some(token) => Some(
            server
                .manager
                .caller(token)
                .await
                .map_err(|error| errors::failed(&error))?
                .ok_or_else(|| {
                    ApiError::new(ErrorCode::ScopeDenied, "expired or unknown workspace scope")
                })?,
        ),
        None => None,
    };
    Ok(Session {
        server: server.clone(),
        scope: hello.scope,
        caller,
    })
}

/// The reply that admits a session.
pub(super) fn welcome() -> HandshakeReply {
    HandshakeReply::Welcome(Welcome {
        version: shoal_client::VERSION.into(),
        supported_majors: SUPPORTED_MAJORS.to_vec(),
        features: Call::NAMES
            .iter()
            .filter(|name| **name != "subscribe")
            .map(|name| (*name).to_owned())
            .collect(),
    })
}
