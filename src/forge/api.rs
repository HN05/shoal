//! Forgejo REST requests with the token fj saved for the host, read per request
//! and passed to curl on stdin, never stored or placed on a command line.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use super::ForgeRepo;
use crate::tools::Tool;

const TIMEOUT: Duration = Duration::from_secs(20);
/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

impl ForgeRepo {
    /// GET a repository endpoint, authenticated when fj has a login for the
    /// host; without one only public repositories are readable.
    pub(super) async fn forgejo_api(&self, endpoint: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}://{}/api/v1/repos/{}/{endpoint}",
            self.web_scheme, self.host, self.path
        );
        let token = fj_keys_path()
            .and_then(|path| fj_token(&path, &self.host))
            .ok();
        let mut curl = Command::new(Tool::Curl.program());
        // -q first: a user .curlrc must not trace the token header.
        curl.args([
            "-q",
            "-sS",
            "-f",
            "-H",
            "Accept: application/json",
            "-K",
            "-",
            &url,
        ]);
        let output = crate::subprocess::Run::new(curl)
            .input(token.map(authorization).unwrap_or_default())
            .timeout(TIMEOUT)
            .output()
            .await
            .with_context(|| {
                format!(
                    "Forgejo API request to {url} failed; private repositories need an fj login"
                )
            })?;
        serde_json::from_str(&output).with_context(|| format!("invalid response from {url}"))
    }

    /// Every item of a paginated list endpoint.
    pub(super) async fn forgejo_api_pages(&self, endpoint: &str) -> Result<Vec<serde_json::Value>> {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let mut items = Vec::new();
        for page in 1.. {
            let response = self
                .forgejo_api(&format!(
                    "{endpoint}{separator}limit={PAGE_SIZE}&page={page}"
                ))
                .await?;
            let batch = response
                .as_array()
                .with_context(|| format!("Forgejo {endpoint} is not a list"))?;
            items.extend(batch.iter().cloned());
            if batch.len() < PAGE_SIZE {
                break;
            }
        }
        Ok(items)
    }
}

/// A curl config line, so the token never appears on a command line.
pub(super) fn authorization(token: String) -> Vec<u8> {
    format!("header = \"Authorization: token {token}\"\n").into_bytes()
}

/// Where fj keeps its logins.
pub(super) fn fj_keys_path() -> Result<PathBuf> {
    let home = crate::fsutil::home_dir()?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/forgejo-cli.forgejo-cli/keys.json")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.join(".local/share"), PathBuf::from)
            .join("forgejo-cli/keys.json")
    })
}

/// The token fj saved for exactly this host.
pub(super) fn fj_token(path: &Path, host: &str) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read fj logins at {}; run fj auth login", path.display()))?;
    let keys: serde_json::Value = serde_json::from_str(&text).context("parse fj logins")?;
    let token = keys["hosts"][host]["token"]
        .as_str()
        .with_context(|| format!("fj has no login for {host}; run fj auth login"))?;
    // The token goes into a curl config line; anything but a plain token is refused.
    ensure!(
        !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "fj's login for {host} has an unexpected token format"
    );
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fj_tokens_are_read_per_host_and_refused_when_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            r#"{"hosts":{"forge.example":{"type":"Application","token":"abc123"},
                "bad.example":{"type":"Application","token":"a\"b"}},"aliases":{}}"#,
        )
        .unwrap();
        assert_eq!(fj_token(&path, "forge.example").unwrap(), "abc123");
        assert!(
            format!("{:#}", fj_token(&path, "other.example").unwrap_err()).contains("no login")
        );
        assert!(fj_token(&path, "bad.example").is_err());
        assert!(fj_token(&dir.path().join("missing"), "forge.example").is_err());
    }
}
