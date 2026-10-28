//! Load Happy's local account and machine context.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;

use super::server::Server;
use crate::paths::Paths;

pub const SERVER_URL_ENV: &str = "HAPPY_SERVER_URL";
const DEFAULT_SERVER_URL: &str = "https://api.cluster-fluster.com";
const CREDENTIALS_FILE: &str = "access.key";
const SETTINGS_FILE: &str = "settings.json";

pub(super) struct Account {
    pub home: PathBuf,
    pub machine_id: String,
    pub encryption: Encryption,
    pub server: Server,
}

impl Account {
    pub fn load(paths: &Paths) -> Result<Self> {
        let home = crate::happy::home(&paths.home);
        let credentials = read_credentials(&home)?;
        let settings = read_settings(&home);
        let machine_id = machine_id(&settings)?;
        Ok(Self {
            home,
            machine_id,
            encryption: credentials.encryption,
            server: Server::new(
                server_url(&settings),
                credentials.token,
                paths.state.clone(),
            ),
        })
    }
}

fn machine_id(settings: &Value) -> Result<String> {
    settings["machineId"]
        .as_str()
        .context("Happy settings have no machine ID; run happy once to register this machine")
        .map(str::to_owned)
}

/// happy-cli's `access.key`: a bearer token plus either the legacy account
/// secret or the account's box public key for per-session data keys.
struct Credentials {
    token: String,
    encryption: Encryption,
}

pub(super) enum Encryption {
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

#[cfg(test)]
mod tests;
