//! Happy session envelopes, creation schema and reconnect variables.
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use uuid::Uuid;

use super::account::Encryption;
use crate::happy::{
    Agent,
    crypto::{self, Variant},
};
use crate::paths::Paths;

/// Variables happy-cli reads to attach to an existing session instead of
/// creating one; its daemon sets them to resume a session in place.
pub const RECONNECT_ENV: [&str; 6] = [
    "HAPPY_RECONNECT_SESSION_ID",
    "HAPPY_RECONNECT_ENCRYPTION_KEY",
    "HAPPY_RECONNECT_ENCRYPTION_VARIANT",
    "HAPPY_RECONNECT_SEQ",
    "HAPPY_RECONNECT_METADATA_VERSION",
    "HAPPY_RECONNECT_AGENT_STATE_VERSION",
];

/// A session Shoal created for the agent to attach to.
pub struct Session {
    pub id: String,
    encryption: SessionEncryption,
    seq: i64,
    metadata_version: i64,
    agent_state_version: i64,
}

impl Session {
    /// Environment that makes `happy <agent>` attach to this session.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let values = [
            self.id.clone(),
            BASE64.encode(self.encryption.key),
            self.encryption.variant.name().to_owned(),
            self.seq.to_string(),
            self.metadata_version.to_string(),
            self.agent_state_version.to_string(),
        ];
        RECONNECT_ENV.into_iter().zip(values).collect()
    }

    pub(super) fn encrypt(&self, value: &Value) -> Result<String> {
        self.encryption.encrypt(value)
    }
}

struct SessionEncryption {
    key: [u8; 32],
    variant: Variant,
}

impl SessionEncryption {
    fn encrypt(&self, value: &Value) -> Result<String> {
        Ok(BASE64.encode(crypto::encrypt(
            &self.key,
            self.variant,
            value.to_string().as_bytes(),
        )?))
    }
}

/// Keep the session key, encryption variant and account envelope together
/// from payload encryption through reconnect response decoding.
pub(super) struct PreparedSession {
    encryption: SessionEncryption,
    sealed_key: Option<String>,
}

impl PreparedSession {
    pub fn prepare(account: &Encryption) -> Result<Self> {
        let (key, variant, sealed_key) = match account {
            Encryption::Legacy(secret) => {
                let key: [u8; 32] = secret.as_slice().try_into()?;
                (key, Variant::Legacy, None)
            }
            Encryption::DataKey { public_key } => {
                let key = crypto::random_key();
                let sealed = crypto::seal_for_account(&key, public_key)?;
                (key, Variant::DataKey, Some(BASE64.encode(sealed)))
            }
        };
        Ok(Self {
            encryption: SessionEncryption { key, variant },
            sealed_key,
        })
    }

    pub fn creation_payload(&self, metadata: &Value) -> Result<Value> {
        Ok(json!({
            "tag": Uuid::new_v4().to_string(),
            "metadata": self.encryption.encrypt(metadata)?,
            "agentState": self.encryption.encrypt(&json!({"controlledByUser": false}))?,
            "dataEncryptionKey": self.sealed_key,
        }))
    }

    pub fn decode_response(self, created: &Value) -> Result<Session> {
        let raw = &created["session"];
        Ok(Session {
            id: raw["id"]
                .as_str()
                .context("Happy server returned no session ID")?
                .to_owned(),
            encryption: self.encryption,
            seq: raw["seq"].as_i64().unwrap_or(0),
            metadata_version: raw["metadataVersion"].as_i64().unwrap_or(0),
            agent_state_version: raw["agentStateVersion"].as_i64().unwrap_or(0),
        })
    }
}

pub(super) fn creation_metadata(
    paths: &Paths,
    happy_home: &Path,
    machine_id: &str,
    directory: &Path,
    branch: Option<&str>,
    agent: Agent,
) -> Value {
    // Mirrors happy-cli's createSessionMetadata; the CLI replaces it with its
    // own record once it connects.
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    json!({
        "path": directory,
        "host": hostname(),
        "version": "unknown",
        "os": platform(),
        "machineId": machine_id,
        "homeDir": paths.home,
        "happyHomeDir": happy_home,
        "startedFromDaemon": true,
        "startedBy": "daemon",
        "lifecycleState": "running",
        "lifecycleStateSince": now_millis,
        "flavor": agent.as_str(),
        "gitBranch": branch,
    })
}

fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the provided buffer.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return "unknown".into();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Node's `os.platform()` spelling, which Happy's app expects.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

#[cfg(test)]
mod tests;
