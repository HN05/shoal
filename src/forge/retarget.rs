//! Change a PR's base branch: the one forge write Shoal makes, for stacked
//! workspaces whose base PR merged. GitHub uses `gh pr edit`. fj cannot edit a
//! base, so Forgejo uses its REST API with the token fj saved for that host,
//! read for this request only and passed to curl on stdin, never stored.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use super::{ForgeKind, ForgeRepo};
use crate::tools::Tool;

const TIMEOUT: Duration = Duration::from_secs(30);

impl ForgeRepo {
    pub(crate) async fn retarget(&self, path: &Path, number: u64, base: &str) -> Result<()> {
        match self.kind {
            ForgeKind::GitHub => self.github_retarget(path, number, base).await,
            ForgeKind::Forgejo => self.forgejo_retarget(number, base).await,
        }
        .with_context(|| format!("could not retarget PR #{number} to {base}"))
    }

    async fn github_retarget(&self, path: &Path, number: u64, base: &str) -> Result<()> {
        let mut gh = Command::new(self.kind.tool());
        let repository = format!("{}/{}", self.host, self.path);
        gh.current_dir(path).env("NO_COLOR", "1").args([
            "pr",
            "edit",
            &number.to_string(),
            "--repo",
            &repository,
            "--base",
            base,
        ]);
        crate::subprocess::Run::new(gh)
            .timeout(TIMEOUT)
            .checked()
            .await?;
        Ok(())
    }

    async fn forgejo_retarget(&self, number: u64, base: &str) -> Result<()> {
        let token = fj_token(&fj_keys_path()?, &self.host)?;
        let url = format!(
            "{}://{}/api/v1/repos/{}/pulls/{number}",
            self.web_scheme, self.host, self.path
        );
        let body = serde_json::json!({ "base": base }).to_string();
        let mut curl = Command::new(Tool::Curl.program());
        curl.args([
            "-sS",
            "-f",
            "-o",
            "/dev/null",
            "--max-time",
            "30",
            "-X",
            "PATCH",
            "-H",
            "Content-Type: application/json",
            "--data",
            &body,
            "-K",
            "-",
            &url,
        ]);
        // curl's config syntax: the token never appears on a command line.
        crate::subprocess::Run::new(curl)
            .input(format!("header = \"Authorization: token {token}\"\n").into_bytes())
            .timeout(TIMEOUT)
            .checked()
            .await
            .context(
                "Forgejo API request failed; curl and an fj login with write access are required",
            )?;
        Ok(())
    }
}

/// Where fj keeps its logins.
fn fj_keys_path() -> Result<PathBuf> {
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
fn fj_token(path: &Path, host: &str) -> Result<String> {
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
