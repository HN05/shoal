//! A request to the forge's API that a caller wrote, for reads no other
//! operation covers.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

shoal_states::states!(
    /// The HTTP methods forge requests use.
    HttpMethod: ValueEnum {
        Get => "GET",
        Post => "POST",
        Patch => "PATCH",
        Put => "PUT",
        Delete => "DELETE",
    }
);

/// A request to a path under the forge's API root, such as
/// `repos/{owner}/{repo}/branches`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRequest {
    pub method: HttpMethod,
    pub path: String,
    pub body: Option<Value>,
}

impl RawRequest {
    /// Refuse a path that is empty, names another host or climbs out of the
    /// API root, before any repository is known.
    pub fn check(&self) -> Result<()> {
        let path = self.path.trim_start_matches('/');
        let route = path.split(['?', '#']).next().unwrap_or_default();
        ensure!(!route.is_empty(), "the API path is empty");
        ensure!(
            !path.contains("://") && !path.chars().any(|c| c.is_whitespace() || c.is_control()),
            "the API path must be relative to the forge's API root, without spaces"
        );
        ensure!(
            !decoded(route)
                .split(['/', '\\'])
                .any(|part| matches!(part, "." | "..")),
            "the API path must not contain `.` or `..` segments"
        );
        Ok(())
    }
}

/// `text` with percent-encoding undone until nothing changes, as servers
/// and the proxies before them may each decode a path before removing its
/// dot segments.
fn decoded(text: &str) -> String {
    let mut text = text.to_owned();
    loop {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            let hex = bytes
                .get(at + 1..at + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            match (bytes[at], hex) {
                (b'%', Some(byte)) => {
                    out.push(byte);
                    at += 3;
                }
                (byte, _) => {
                    out.push(byte);
                    at += 1;
                }
            }
        }
        let next = String::from_utf8_lossy(&out).into_owned();
        if next == text {
            return text;
        }
        text = next;
    }
}
