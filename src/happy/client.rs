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
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use tokio::{
    process::Command,
    time::{Instant, sleep},
};
use uuid::Uuid;

use super::{
    HappyAgent,
    crypto::{self, Variant},
};
use crate::{model::Workspace, paths::Paths};

pub const SERVER_URL_ENV: &str = "HAPPY_SERVER_URL";
const DEFAULT_SERVER_URL: &str = "https://api.cluster-fluster.com";
const CREDENTIALS_FILE: &str = "access.key";
const SETTINGS_FILE: &str = "settings.json";
const CLIENT_HEADER: &str = concat!("X-Happy-Client: shoal/", env!("CARGO_PKG_VERSION"));

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

/// happy-cli's `access.key`: a bearer token plus either the legacy account
/// secret or the account's box public key for per-session data keys.
struct Credentials {
    token: String,
    encryption: Encryption,
}

enum Encryption {
    Legacy(Vec<u8>),
    DataKey { public_key: Vec<u8> },
}

fn read_credentials(happy_home: &Path) -> Result<Credentials> {
    let path = happy_home.join(CREDENTIALS_FILE);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "Happy is not logged in on this machine (no {}); run happy once to authenticate",
            path.display()
        )
    })?;
    parse_credentials(&text).with_context(|| format!("parse {}", path.display()))
}

fn parse_credentials(text: &str) -> Result<Credentials> {
    let raw: Value = serde_json::from_str(text)?;
    let token = raw["token"]
        .as_str()
        .context("credentials have no token")?
        .to_owned();
    let encryption = if let Some(secret) = raw["secret"].as_str() {
        let secret = BASE64
            .decode(secret)
            .context("account secret is not base64")?;
        ensure!(secret.len() == 32, "account secret must be 32 bytes");
        Encryption::Legacy(secret)
    } else if let Some(key) = raw["encryption"]["publicKey"].as_str() {
        let public_key = BASE64
            .decode(key)
            .context("account public key is not base64")?;
        ensure!(
            public_key.len() == 32,
            "account public key must be 32 bytes"
        );
        Encryption::DataKey { public_key }
    } else {
        bail!("credentials have neither a secret nor an encryption public key");
    };
    Ok(Credentials { token, encryption })
}

fn read_settings(happy_home: &Path) -> Value {
    std::fs::read_to_string(happy_home.join(SETTINGS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// happy-cli's precedence: environment, then settings, then the default.
fn server_url(settings: &Value) -> String {
    std::env::var(SERVER_URL_ENV)
        .ok()
        .filter(|url| !url.is_empty())
        .or_else(|| settings["serverUrl"].as_str().map(str::to_owned))
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// A Happy server and the account token, spoken through `curl`.
struct Server {
    url: String,
    token: String,
    /// Private directory for request bodies (`--data-binary @file`).
    temp_dir: PathBuf,
}

impl Server {
    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut curl = Command::new("curl");
        curl.args([
            "-sS",
            "-f",
            "--max-time",
            "30",
            "-X",
            method,
            "-H",
            "Content-Type: application/json",
            "-H",
            CLIENT_HEADER,
            "-K",
            "-",
        ]);
        let _body_file = match body {
            Some(body) => {
                let mut file = tempfile::NamedTempFile::new_in(&self.temp_dir)
                    .context("create request body file")?;
                serde_json::to_writer(&mut file, body)?;
                curl.arg("--data-binary");
                curl.arg(format!("@{}", file.path().display()));
                Some(file)
            }
            None => None,
        };
        curl.arg(format!("{}{path}", self.url));
        // curl's config syntax: the token never appears on a command line.
        let output = crate::subprocess::Run::new(curl)
            .input(format!("header = \"Authorization: Bearer {}\"\n", self.token).into_bytes())
            .timeout(Duration::from_secs(30))
            .checked()
            .await
            .with_context(|| {
                format!("Happy server request {method} {path} failed; curl is required")
            })?;
        if output.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("Happy server returned invalid JSON for {method} {path}"))
    }
}

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

/// Create a session for `agent` in `workspace` on Happy's server, encrypted
/// the way happy-cli would have encrypted it for this account.
pub async fn seed(paths: &Paths, workspace: &Workspace, agent: HappyAgent) -> Result<Seeded> {
    let happy_home = super::home(&paths.home);
    let credentials = read_credentials(&happy_home)?;
    let settings = read_settings(&happy_home);
    let machine_id = settings["machineId"]
        .as_str()
        .context("Happy settings have no machine ID; run happy once to register this machine")?;
    let (key, variant, sealed_key) = match &credentials.encryption {
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    let metadata = json!({
        "path": workspace.path,
        "host": hostname(),
        "version": "unknown",
        "os": platform(),
        "machineId": machine_id,
        "homeDir": paths.home,
        "happyHomeDir": happy_home,
        "startedFromDaemon": true,
        "startedBy": "daemon",
        "lifecycleState": "running",
        "lifecycleStateSince": now,
        "flavor": agent.name(),
        "gitBranch": workspace.branch,
    });
    let body = json!({
        "tag": Uuid::new_v4().to_string(),
        "metadata": session.encrypt(&metadata)?,
        "agentState": session.encrypt(&json!({"controlledByUser": false}))?,
        "dataEncryptionKey": sealed_key,
    });
    let server = Server {
        url: server_url(&settings),
        token: credentials.token,
        temp_dir: paths.state.clone(),
    };
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
mod tests {
    use super::*;

    #[test]
    fn credentials_accept_legacy_secrets_and_data_key_public_keys() {
        let key = BASE64.encode([7u8; 32]);
        let legacy = parse_credentials(&format!(r#"{{"token":"t","secret":"{key}"}}"#)).unwrap();
        assert_eq!(legacy.token, "t");
        assert!(matches!(legacy.encryption, Encryption::Legacy(ref s) if s == &[7u8; 32]));
        let data_key = parse_credentials(&format!(
            r#"{{"token":"t","secret":null,"encryption":{{"publicKey":"{key}","machineKey":"{key}"}}}}"#
        ))
        .unwrap();
        assert!(
            matches!(data_key.encryption, Encryption::DataKey { ref public_key } if public_key == &[7u8; 32])
        );
        assert!(parse_credentials(r#"{"token":"t"}"#).is_err());
        assert!(parse_credentials(r#"{"secret":"AAAA"}"#).is_err());
        let short = BASE64.encode([1u8; 16]);
        assert!(parse_credentials(&format!(r#"{{"token":"t","secret":"{short}"}}"#)).is_err());
    }

    #[test]
    fn server_url_prefers_settings_over_the_default() {
        if std::env::var_os(SERVER_URL_ENV).is_some() {
            return;
        }
        assert_eq!(server_url(&Value::Null), DEFAULT_SERVER_URL);
        assert_eq!(
            server_url(&json!({"serverUrl": "http://localhost:3005/"})),
            "http://localhost:3005"
        );
    }

    #[test]
    fn session_env_uses_happy_cli_reconnect_variables() {
        let session = Session {
            id: "s1".into(),
            key: [9u8; 32],
            variant: Variant::DataKey,
            seq: 3,
            metadata_version: 1,
            agent_state_version: 2,
        };
        let env = session.env();
        assert_eq!(env[0], ("HAPPY_RECONNECT_SESSION_ID", "s1".to_owned()));
        assert_eq!(env[1].1, BASE64.encode([9u8; 32]));
        assert_eq!(env[2].1, "dataKey");
        assert_eq!(env[3].1, "3");
        assert_eq!(env[4].1, "1");
        assert_eq!(env[5].1, "2");
        assert!(!platform().is_empty());
    }
}
