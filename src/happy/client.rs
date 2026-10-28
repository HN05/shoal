//! Seed a Happy session on Happy's server and hand the agent its first prompt.
//!
//! `happy codex` has no prompt argument; the app delivers the first message
//! through Happy's server after the session connects. Shoal does the same:
//! it creates the session with the account credentials happy-cli stores,
//! launches the CLI attached to that session through the reconnection
//! variables its daemon uses for resume-in-place, waits for the session to
//! report alive, and posts the prompt. HTTP goes through `curl` with the
//! token on stdin, never on the command line.
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use super::Agent;
use super::crypto::{self, Variant};
use crate::paths::Paths;

mod account;
mod server;

use account::{Account, Encryption};
use server::Server;

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
    key: [u8; 32],
    variant: Variant,
    seq: i64,
    metadata_version: i64,
    agent_state_version: i64,
}

impl Session {
    /// Environment that makes `happy <agent>` attach to this session.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let values = [
            self.id.clone(),
            BASE64.encode(self.key),
            self.variant.name().to_owned(),
            self.seq.to_string(),
            self.metadata_version.to_string(),
            self.agent_state_version.to_string(),
        ];
        RECONNECT_ENV.into_iter().zip(values).collect()
    }

    fn encrypt(&self, value: &Value) -> Result<String> {
        Ok(BASE64.encode(crypto::encrypt(
            &self.key,
            self.variant,
            value.to_string().as_bytes(),
        )?))
    }
}

/// A seeded session and the server that holds it.
pub struct Seeded {
    server: Server,
    pub session: Session,
}

/// Create a session for `agent` in `directory`, on `branch` when it has one,
/// on Happy's server, encrypted the way happy-cli would have encrypted it for
/// this account.
pub async fn seed(
    paths: &Paths,
    directory: &Path,
    branch: Option<&str>,
    agent: Agent,
) -> Result<Seeded> {
    let account = Account::load(paths)?;
    let (key, variant, sealed_key) = match &account.encryption {
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
    let mut session = Session {
        id: String::new(),
        key,
        variant,
        seq: 0,
        metadata_version: 0,
        agent_state_version: 0,
    };
    // Mirrors happy-cli's createSessionMetadata; the CLI replaces it with its
    // own record once it connects.
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    let metadata = json!({
        "path": directory,
        "host": hostname(),
        "version": "unknown",
        "os": platform(),
        "machineId": account.machine_id,
        "homeDir": paths.home,
        "happyHomeDir": account.home,
        "startedFromDaemon": true,
        "startedBy": "daemon",
        "lifecycleState": "running",
        "lifecycleStateSince": now_millis,
        "flavor": agent.as_str(),
        "gitBranch": branch,
    });
    let body = json!({
        "tag": Uuid::new_v4().to_string(),
        "metadata": session.encrypt(&metadata)?,
        "agentState": session.encrypt(&json!({"controlledByUser": false}))?,
        "dataEncryptionKey": sealed_key,
    });
    let server = account.server;
    let created = server.request("POST", "/v1/sessions", Some(&body)).await?;
    let raw = &created["session"];
    session.id = raw["id"]
        .as_str()
        .context("Happy server returned no session ID")?
        .to_owned();
    session.seq = raw["seq"].as_i64().unwrap_or(0);
    session.metadata_version = raw["metadataVersion"].as_i64().unwrap_or(0);
    session.agent_state_version = raw["agentStateVersion"].as_i64().unwrap_or(0);
    Ok(Seeded { server, session })
}

impl Seeded {
    /// Post `text` as the user's first message once the agent's session is
    /// alive, so the CLI receives it live rather than as skipped history.
    pub async fn deliver(&self, text: &str, timeout: Duration) -> Result<()> {
        self.wait_until_active(timeout).await?;
        let message = json!({
            "role": "user",
            "content": {"type": "text", "text": text},
            "meta": {"sentFrom": "shoal"},
        });
        let body = json!({
            "messages": [{
                "localId": Uuid::new_v4().to_string(),
                "content": self.session.encrypt(&message)?,
            }]
        });
        self.server
            .request(
                "POST",
                &format!("/v3/sessions/{}/messages", self.session.id),
                Some(&body),
            )
            .await?;
        Ok(())
    }

    /// Delete a session nothing will attach to. Best effort: the launch error
    /// being reported matters more than a cleanup failure.
    pub async fn discard(self) {
        let path = format!("/v1/sessions/{}", self.session.id);
        if let Err(error) = self.server.request("DELETE", &path, None).await {
            eprintln!(
                "warning: could not delete the unused Happy session {}: {error:#}",
                self.session.id
            );
        }
    }

    async fn wait_until_active(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let active = self
                .server
                .request("GET", "/v2/sessions/active", None)
                .await?;
            let connected = active["sessions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|session| session["id"] == self.session.id.as_str());
            if connected {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Happy session {} did not connect to the server within {}s",
                self.session.id,
                timeout.as_secs()
            );
            sleep(Duration::from_secs(1)).await;
        }
    }
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
