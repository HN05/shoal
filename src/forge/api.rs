//! Forge REST requests sent as an [`Account`]: GitHub through `gh api`, which
//! owns its login, and Forgejo through curl with the token fj saved for the
//! host, read per request and passed on stdin, never placed on a command line.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tokio::process::Command;

use super::{ForgeKind, ForgeRepo, account::Account};
use crate::tools::Tool;

const TIMEOUT: Duration = Duration::from_secs(30);
/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl HttpMethod {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// One REST request; `endpoint` is relative to the forge's API root.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Request {
    pub method: HttpMethod,
    pub endpoint: String,
    pub body: Option<Value>,
}

impl Request {
    pub fn get(endpoint: String) -> Self {
        Self::new(HttpMethod::Get, endpoint, None)
    }

    pub fn new(method: HttpMethod, endpoint: String, body: Option<Value>) -> Self {
        Self {
            method,
            endpoint,
            body,
        }
    }
}

impl ForgeRepo {
    /// The API endpoint for `path` under this repository.
    pub(crate) fn repo_endpoint(&self, path: &str) -> String {
        format!("repos/{}/{path}", self.path)
    }

    /// The response body, or null for an empty one.
    pub(crate) async fn send(&self, account: &Account, request: &Request) -> Result<Value> {
        match self.kind {
            ForgeKind::GitHub => self.github_send(account, request).await,
            ForgeKind::Forgejo => self.forgejo_send(account, request).await,
        }
        .with_context(|| format!("{} {}", request.method.as_str(), request.endpoint))
    }

    async fn github_send(&self, account: &Account, request: &Request) -> Result<Value> {
        let mut gh = Command::new(Tool::GitHub.program());
        gh.env("NO_COLOR", "1")
            .env("GH_PROMPT_DISABLED", "1")
            .args([
                "api",
                "--hostname",
                &self.host,
                "-X",
                request.method.as_str(),
                &request.endpoint,
            ]);
        if request.body.is_some() {
            gh.args(["--input", "-"]);
        }
        account.apply(&mut gh);
        let body = request.body.as_ref().map(Value::to_string);
        let output = crate::subprocess::Run::new(gh)
            .input(body.unwrap_or_default())
            .timeout(TIMEOUT)
            .capture()
            .await
            .context("run gh; is it installed?")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            bail!(
                "{}",
                error_message(&stdout)
                    .unwrap_or_else(|| crate::subprocess::diagnostic(&output.stderr))
                    .trim()
            );
        }
        json_body(&stdout)
    }

    async fn forgejo_send(&self, account: &Account, request: &Request) -> Result<Value> {
        let url = format!(
            "{}://{}/api/v1/{}",
            self.web_scheme, self.host, request.endpoint
        );
        let mut curl = Command::new(Tool::Curl.program());
        // -q first: a user .curlrc must not trace the token header.
        curl.args([
            "-q",
            "-sS",
            "--max-time",
            "30",
            "-X",
            request.method.as_str(),
            "-H",
            "Accept: application/json",
            "-w",
            "\n%{http_code}",
        ]);
        if let Some(body) = &request.body {
            curl.args(["-H", "Content-Type: application/json", "--data-binary"])
                .arg(body.to_string());
        }
        curl.args(["-K", "-", &url]);
        let token = account.forgejo_token(&self.host)?;
        let output = crate::subprocess::Run::new(curl)
            .input(token.map(authorization).unwrap_or_default())
            .timeout(TIMEOUT)
            .output()
            .await
            .context("curl is required for Forgejo API requests")?;
        forgejo_response(&output)
    }

    /// GET a repository endpoint, authenticated when fj has a login for the
    /// host; without one only public repositories are readable.
    pub(super) async fn forgejo_api(&self, endpoint: &str) -> Result<Value> {
        self.send(
            &Account::user(),
            &Request::get(self.repo_endpoint(endpoint)),
        )
        .await
        .context("Forgejo API request failed; private repositories need an fj login")
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

/// A JSON body, or null for an empty one.
fn json_body(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).context("invalid JSON response")
}

/// The message a forge's JSON error body carries.
fn error_message(text: &str) -> Option<String> {
    let body: Value = serde_json::from_str(text).ok()?;
    let message = body["message"].as_str()?;
    let details: Vec<_> = body["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|error| error["message"].as_str().or(error.as_str()))
        .collect();
    Some(if details.is_empty() {
        message.to_owned()
    } else {
        format!("{message}: {}", details.join("; "))
    })
}

/// curl's output ends with the status code `-w` appends on its own line.
fn forgejo_response(text: &str) -> Result<Value> {
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text));
    let status: u16 = status
        .trim()
        .parse()
        .context("Forgejo response has no HTTP status")?;
    if status >= 400 {
        let detail = error_message(body).unwrap_or_else(|| body.chars().take(500).collect());
        bail!("HTTP {status}: {}", detail.trim());
    }
    json_body(body)
}

/// A curl config line, so the token never appears on a command line.
pub(super) fn authorization(token: String) -> Vec<u8> {
    format!("header = \"Authorization: token {token}\"\n").into_bytes()
}

/// Where fj keeps the user's logins.
pub(super) fn fj_keys_path() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    Ok(fj_keys_at(&crate::fsutil::home_dir()?, data))
}

/// Where fj run with `home` keeps its logins; `data` is its XDG data
/// directory, which Linux honors.
pub(super) fn fj_keys_at(home: &Path, data: Option<PathBuf>) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/forgejo-cli.forgejo-cli/keys.json")
    } else {
        data.unwrap_or_else(|| home.join(".local/share"))
            .join("forgejo-cli/keys.json")
    }
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
    fn forgejo_responses_report_status_and_messages() {
        assert_eq!(
            forgejo_response("{\"number\":7}\n201").unwrap(),
            serde_json::json!({"number": 7})
        );
        assert_eq!(forgejo_response("\n204").unwrap(), Value::Null);
        let error = forgejo_response("{\"message\":\"user does not exist\"}\n404").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 404: user does not exist");
        let error = forgejo_response("<html>busy</html>\n502").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 502: <html>busy</html>");
        assert!(forgejo_response("{}").is_err());
    }

    #[test]
    fn github_errors_name_each_validation_failure() {
        let body = r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists"},"base invalid"]}"#;
        assert_eq!(
            error_message(body).unwrap(),
            "Validation Failed: A pull request already exists; base invalid"
        );
        assert_eq!(error_message("not json"), None);
    }

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
