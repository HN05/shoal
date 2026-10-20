//! Forge REST requests sent as an [`Account`]: GitHub through `gh api`, which
//! owns its login, and Forgejo or Gitea through curl with the token the
//! account's command prints for the host, passed on stdin, never placed on a
//! command line.
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tokio::process::Command;

use super::{ForgeKind, ForgeRepo, account::Account};
use crate::tools::Tool;

pub(super) const TIMEOUT: Duration = Duration::from_secs(30);
/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;
/// Bitbucket Cloud's API, which is not served from its web host.
const BITBUCKET_API: &str = "https://api.bitbucket.org/2.0";
const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
/// Bounds a JSON response read through a CLI, which curl's own limit cannot.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

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
    /// A response field that must be `true` for the request to have worked.
    pub confirm: Option<&'static str>,
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
            confirm: None,
        }
    }

    pub fn confirming(mut self, field: &'static str) -> Self {
        self.confirm = Some(field);
        self
    }
}

impl ForgeRepo {
    /// Read plaintext logs without following redirects or exposing the login.
    pub(super) async fn forgejo_log(&self, account: &Account, endpoint: &str) -> Result<String> {
        let token = account.forgejo_token(&self.host).await?;
        let url = format!("{}://{}/api/v1/{endpoint}", self.web_scheme, self.host);
        let mut curl = Command::new(Tool::Curl.program());
        curl.args([
            "-q",
            "-sS",
            "--max-time",
            "30",
            "--max-filesize",
            "8388608",
            "-H",
            "Accept: text/plain",
            "-w",
            "\n%{http_code}",
            "-K",
            "-",
            &url,
        ]);
        let output = crate::subprocess::Run::new(curl)
            .input(token.map(authorization).unwrap_or_default())
            .timeout(TIMEOUT)
            // curl appends a newline and a three-digit HTTP status.
            .output_limit(MAX_LOG_BYTES + 4)
            .output()
            .await
            .context("fetch Forgejo job log (maximum 8 MiB)")?;
        Ok(curl_body(&output)?.to_owned())
    }

    /// The API endpoint for `path` under this repository.
    pub(crate) fn repo_endpoint(&self, path: &str) -> String {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => format!("repos/{}/{path}", self.path),
            ForgeKind::GitLab => format!("projects/{}/{path}", encode(&self.path, false)),
            ForgeKind::Bitbucket => format!("repositories/{}/{path}", self.path),
        }
    }

    /// The response body, or null for an empty one.
    pub(crate) async fn send(&self, account: &Account, request: &Request) -> Result<Value> {
        async {
            let body = match self.kind {
                ForgeKind::GitHub => self.github_send(account, request).await,
                ForgeKind::Forgejo => self.forgejo_send(account, request).await,
                ForgeKind::GitLab => self.passthrough(Tool::GitLab, account, request).await,
                ForgeKind::Bitbucket => self.bitbucket_send(account, request).await,
            }?;
            if let Some(field) = request.confirm {
                ensure!(
                    body[field] == true,
                    "{}",
                    body["message"]
                        .as_str()
                        .unwrap_or("the forge did not confirm the change")
                );
            }
            Ok(body)
        }
        .await
        .with_context(|| format!("{} {}", request.method.as_str(), request.endpoint))
    }

    async fn github_send(&self, account: &Account, request: &Request) -> Result<Value> {
        let body = self.passthrough(Tool::GitHub, account, request).await?;
        // GraphQL reports a failed mutation in a successful response.
        if let Some(errors) = graphql_errors(&body) {
            bail!("{errors}");
        }
        Ok(body)
    }

    /// Send through `gh api` or `glab api`, which own their logins.
    async fn passthrough(&self, tool: Tool, account: &Account, request: &Request) -> Result<Value> {
        let program = tool.program();
        let mut cli = Command::new(program);
        cli.env("NO_COLOR", "1")
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
            cli.args(["-H", "Content-Type: application/json", "--input", "-"]);
        }
        account.apply(&mut cli);
        let body = request.body.as_ref().map(Value::to_string);
        let output = crate::subprocess::Run::new(cli)
            .input(body.unwrap_or_default())
            .timeout(TIMEOUT)
            .output_limit(MAX_RESPONSE_BYTES)
            .capture()
            .await
            .with_context(|| format!("run {program}; is it installed?"))?;
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
        let token = account.forgejo_token(&self.host).await?;
        self.curl_send(
            &self.forgejo_url(request),
            token.map(authorization),
            request,
        )
        .await
    }

    fn forgejo_url(&self, request: &Request) -> String {
        format!(
            "{}://{}/api/v1/{}",
            self.web_scheme, self.host, request.endpoint
        )
    }

    async fn bitbucket_send(&self, account: &Account, request: &Request) -> Result<Value> {
        let url = format!("{BITBUCKET_API}/{}", request.endpoint);
        let authorization = account.bitbucket_authorization().await?;
        self.curl_send(&url, authorization, request).await
    }

    /// Send with curl, passing the authorization header on stdin so no
    /// command line shows it.
    async fn curl_send(
        &self,
        url: &str,
        authorization: Option<Vec<u8>>,
        request: &Request,
    ) -> Result<Value> {
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
        curl.args(["-K", "-", url]);
        let output = crate::subprocess::Run::new(curl)
            .input(authorization.unwrap_or_default())
            .timeout(TIMEOUT)
            .output()
            .await
            .context("curl is required for Forgejo, Gitea and Bitbucket API requests")?;
        curl_response(&output)
    }

    /// Every item of a paginated list endpoint, read as `account`.
    pub(super) async fn pages(&self, account: &Account, endpoint: &str) -> Result<Vec<Value>> {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let (size, page_size) = match self.kind {
            ForgeKind::Forgejo => ("limit", PAGE_SIZE),
            ForgeKind::GitHub | ForgeKind::GitLab => ("per_page", 100),
            ForgeKind::Bitbucket => ("pagelen", PAGE_SIZE),
        };
        let mut items = Vec::new();
        for page in 1.. {
            let request = Request::get(format!(
                "{endpoint}{separator}{size}={page_size}&page={page}"
            ));
            let response = self.send(account, &request).await?;
            let (batch, more) = self
                .page(&response, page_size)
                .with_context(|| format!("{endpoint} is not a list"))?;
            items.extend(batch.iter().cloned());
            if !more {
                break;
            }
        }
        Ok(items)
    }

    /// A page's items, and whether another page follows: Bitbucket links the
    /// next page, and other forges return a full page until the last.
    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        match self.kind {
            ForgeKind::Bitbucket => {
                Some((response["values"].as_array()?, !response["next"].is_null()))
            }
            ForgeKind::GitHub | ForgeKind::Forgejo | ForgeKind::GitLab => {
                let batch = response.as_array()?;
                Some((batch, batch.len() >= size))
            }
        }
    }
}

/// Percent-encode `text` for a URL path or query, keeping `/` when `slash`.
pub(crate) fn encode(text: &str, slash: bool) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (slash && byte == b'/') {
            encoded.push(byte.into());
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A JSON body, or null for an empty one.
fn json_body(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).context("invalid JSON response")
}

/// The messages of a GraphQL response's `errors`, when it has any.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())?;
    Some(
        errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// The message a forge's JSON error body carries; GitLab reports invalid
/// fields as a map of messages, and some failures as `error`.
fn error_message(text: &str) -> Option<String> {
    let body: Value = serde_json::from_str(text).ok()?;
    if let Some(fields) = body["message"].as_object() {
        let fields: Vec<_> = fields
            .iter()
            .map(|(field, errors)| match errors.as_array() {
                Some(errors) => format!(
                    "{field} {}",
                    errors
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => format!("{field}: {errors}"),
            })
            .collect();
        return Some(fields.join("; "));
    }
    let message = body["message"]
        .as_str()
        .or(body["error"].as_str())
        .or(body["error"]["message"].as_str())?;
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
fn curl_response(text: &str) -> Result<Value> {
    json_body(curl_body(text)?)
}

fn curl_body(text: &str) -> Result<&str> {
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text));
    let status: u16 = status
        .trim()
        .parse()
        .context("response has no HTTP status")?;
    http_body(status, body)
}

/// The body of a successful response, or the error a failed one reports.
pub(super) fn http_body(status: u16, body: &str) -> Result<&str> {
    if !(200..300).contains(&status) {
        let detail = error_message(body).unwrap_or_else(|| body.chars().take(500).collect());
        bail!("HTTP {status}: {}", detail.trim());
    }
    Ok(body)
}

/// A curl config line, so the token never appears on a command line.
pub(super) fn authorization(token: String) -> Vec<u8> {
    format!("header = \"Authorization: token {token}\"\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgejo_responses_report_status_and_messages() {
        assert_eq!(
            curl_response("{\"number\":7}\n201").unwrap(),
            serde_json::json!({"number": 7})
        );
        assert_eq!(curl_response("\n204").unwrap(), Value::Null);
        let error = curl_response("{\"message\":\"user does not exist\"}\n404").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 404: user does not exist");
        let error = curl_response("<html>busy</html>\n502").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 502: <html>busy</html>");
        assert!(curl_response("{}").is_err());
    }

    #[test]
    fn graphql_errors_fail_a_successful_response() {
        let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
        assert_eq!(
            graphql_errors(&failed).unwrap(),
            "not allowed; GraphQL error"
        );
        assert_eq!(
            graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
            None
        );
        assert_eq!(graphql_errors(&serde_json::json!([])), None);
    }

    #[test]
    fn pages_end_at_a_short_page_or_without_a_next_link() {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let full = serde_json::json!([1, 2]);
        assert!(forgejo.page(&full, 2).unwrap().1);
        assert!(!forgejo.page(&full, 3).unwrap().1);
        let bitbucket = ForgeRepo::parse("https://bitbucket.org/workspace/repo").unwrap();
        let linked =
            serde_json::json!({"values": [1], "next": "https://api.bitbucket.org/2.0/x?page=2"});
        assert_eq!(
            bitbucket.page(&linked, 50).unwrap(),
            (&[1.into()][..], true)
        );
        let last = serde_json::json!({"values": [1]});
        assert!(!bitbucket.page(&last, 1).unwrap().1);
        assert!(bitbucket.page(&full, 2).is_none());
    }

    #[test]
    fn encoding_keeps_unreserved_characters() {
        assert_eq!(encode("feat/a b#1", true), "feat/a%20b%231");
        assert_eq!(encode("feat/ü", false), "feat%2F%C3%BC");
    }

    #[test]
    fn github_errors_name_each_validation_failure() {
        let body = r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists"},"base invalid"]}"#;
        assert_eq!(
            error_message(body).unwrap(),
            "Validation Failed: A pull request already exists; base invalid"
        );
        assert_eq!(error_message("not json"), None);
        let gitlab = r#"{"message":{"title":["can't be blank","is too short"],"base":"bad"}}"#;
        assert_eq!(
            error_message(gitlab).unwrap(),
            "base: \"bad\"; title can't be blank, is too short"
        );
        assert_eq!(
            error_message(r#"{"error":"insufficient_scope"}"#).unwrap(),
            "insufficient_scope"
        );
        assert_eq!(
            error_message(r#"{"type":"error","error":{"message":"Repository not found"}}"#)
                .unwrap(),
            "Repository not found"
        );
    }
}
