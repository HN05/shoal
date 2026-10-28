//! Seed a Happy session on Happy's server and hand the agent its first prompt.
//!
//! `happy codex` has no prompt argument; the app delivers the first message
//! through Happy's server after the session connects. Shoal does the same:
//! it creates the session with the account credentials happy-cli stores,
//! launches the CLI attached to that session through the reconnection
//! variables its daemon uses for resume-in-place, waits for the session to
//! report alive, and posts the prompt. HTTP goes through `curl` with the
//! token on stdin, never on the command line.
use std::{path::Path, time::Duration};

use anyhow::{Result, ensure};
use serde_json::json;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use super::Agent;
use crate::paths::Paths;

mod account;
mod server;
mod session;

use account::Account;
use server::Server;
use session::PreparedSession;
pub use session::{RECONNECT_ENV, Session};

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
    let prepared = PreparedSession::prepare(&account.encryption)?;
    let metadata = session::creation_metadata(
        paths,
        &account.home,
        &account.machine_id,
        directory,
        branch,
        agent,
    );
    let body = prepared.creation_payload(&metadata)?;
    let server = account.server;
    let created = server.create_session(&body).await?;
    let session = prepared.decode_response(&created)?;
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
