//! REST requests and the ways backends send them as an [`Account`]: through
//! a CLI's `api` command, which owns its login, or through curl with a token
//! passed on stdin, never placed on a command line.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tokio::process::Command;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    transport::{
        limits::unix_seconds,
        programs,
        resend::{Resend, Transient, resending},
        response::{Response, last_head},
    },
};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
/// Bounds sending an attached file, which may be a large video.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Bounds a JSON response read through a CLI, which curl's own limit cannot.
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

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
    /// A file sent as the body instead of JSON.
    pub upload: Option<Upload>,
    pub resend: Resend,
    /// Header lines that make a read conditional on the body remembered
    /// for it.
    pub conditions: Vec<String>,
}

/// A file a request sends: as a multipart form's `field`, or as the raw
/// body without one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Upload {
    pub path: PathBuf,
    pub content_type: &'static str,
    pub field: Option<&'static str>,
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
            upload: None,
            resend: Resend::Never,
            conditions: Vec::new(),
        }
    }

    /// POST the file `upload` names, again if the forge did not get it.
    pub fn upload(endpoint: String, upload: Upload) -> Self {
        Self {
            upload: Some(upload),
            resend: Resend::Unreceived,
            ..Self::new(HttpMethod::Post, endpoint, None)
        }
    }

    /// Send again after any failure that may pass, since sending it twice
    /// does no more than sending it once.
    pub fn repeatable(mut self) -> Self {
        self.resend = Resend::Always;
        self
    }

    fn timeout(&self) -> Duration {
        if self.upload.is_some() {
            UPLOAD_TIMEOUT
        } else {
            TIMEOUT
        }
    }

    pub fn confirming(mut self, field: &'static str) -> Self {
        self.confirm = Some(field);
        self
    }

    /// This request, sent with the header lines `conditions`.
    fn conditional(&self, conditions: Vec<String>) -> Self {
        Self {
            conditions,
            ..self.clone()
        }
    }
}

impl ForgeRepo {
    /// The API endpoint for `path` under this repository.
    pub(crate) fn repo_endpoint(&self, path: &str) -> String {
        self.kind.repo_endpoint(self, path)
    }

    /// The response body, or null for an empty one.
    pub(crate) async fn send(&self, account: &Account, request: &Request) -> Result<Value> {
        async {
            let body = resending(request.resend, || self.exchange(account, request)).await?;
            if let Some(failure) = self.kind.reported_failure(&body) {
                bail!("{failure}");
            }
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

    /// The body of the response to `request`, refused without sending it
    /// while the forge limits the account's requests. A read the account
    /// made before is conditional on the body it got, which stands for the
    /// response when the forge reports it unchanged.
    async fn exchange(&self, account: &Account, request: &Request) -> Result<Value> {
        let read = request.method == HttpMethod::Get;
        let url = format!("{}/{}", self.host, request.endpoint);
        let memory = account.memory();
        let now = unix_seconds();
        memory.check_limit(&self.host, now)?;
        let recalled = read.then(|| memory.recall(&url)).flatten();
        let response = match &recalled {
            Some((conditions, _)) => {
                let conditional = request.conditional(conditions.clone());
                self.kind.send(self, account, &conditional).await?
            }
            None => self.kind.send(self, account, request).await?,
        };
        // Relative limits count from the response, which may have taken a while.
        memory.observe_limit(&self.host, &response, unix_seconds())?;
        if let (304, Some((_, body))) = (response.status, recalled) {
            return Ok(body);
        }
        let body = json_body(response.success_body()?)?;
        if read {
            memory.remember(&url, &response, &body);
        }
        Ok(body)
    }

    /// Send through `gh api` or `glab api`, which own their logins and
    /// print the response head before its body.
    pub(crate) async fn passthrough(
        &self,
        program: &'static str,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
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
                "--include",
            ]);
        for condition in &request.conditions {
            cli.args(["-H", condition]);
        }
        let mut input = Vec::new();
        match &request.upload {
            // The CLIs cannot build a form, so its body is sent as given.
            Some(upload @ Upload { field: Some(_), .. }) => {
                let (content_type, form) = multipart(upload).await?;
                cli.args([
                    "-H",
                    &format!("Content-Type: {content_type}"),
                    "--input",
                    "-",
                ]);
                input = form;
            }
            Some(upload) => {
                cli.args(["-H", &format!("Content-Type: {}", upload.content_type)])
                    .arg("--input")
                    .arg(&upload.path);
            }
            None => {
                if let Some(body) = &request.body {
                    cli.args(["-H", "Content-Type: application/json", "--input", "-"]);
                    input = body.to_string().into_bytes();
                }
            }
        }
        account.apply(&mut cli);
        let output = shoal_subprocess::Run::new(cli)
            .input(input)
            .timeout(request.timeout())
            .output_limit(MAX_RESPONSE_BYTES)
            .capture()
            .await
            .with_context(|| format!("run {program}; is it installed?"))?;
        // The CLIs exit with an error for a failed response, which has a head.
        match Response::included(&String::from_utf8_lossy(&output.stdout)) {
            Err(_) if !output.status.success() => {
                bail!("{}", shoal_subprocess::diagnostic(&output.stderr).trim())
            }
            response => response,
        }
    }

    /// Send with curl, passing the authorization header on stdin so no
    /// command line shows it.
    pub(crate) async fn curl_send(
        &self,
        url: &str,
        authorization: Option<Vec<u8>>,
        request: &Request,
    ) -> Result<Response> {
        let mut curl = Command::new(programs::CURL);
        // -q first: a user .curlrc must not trace the token header.
        curl.args(["-q", "-sS", "--max-time"])
            .arg(request.timeout().as_secs().to_string())
            .args([
                "-X",
                request.method.as_str(),
                "-H",
                "Accept: application/json",
                "-w",
                "\n%{http_code}",
                // The head goes apart from the body, which ends with the status.
                "-D",
                "/dev/stderr",
            ]);
        for condition in &request.conditions {
            curl.args(["-H", condition]);
        }
        if let Some(upload) = &request.upload {
            curl.args(upload_args(upload)?);
        } else if let Some(body) = &request.body {
            curl.args(["-H", "Content-Type: application/json", "--data-binary"])
                .arg(body.to_string());
        }
        curl.args(["-K", "-", url]);
        let output = shoal_subprocess::Run::new(curl)
            .input(authorization.unwrap_or_default())
            .timeout(request.timeout())
            .capture()
            .await
            .map_err(curl_unrun)?;
        if !output.status.success() {
            return Err(curl_failure(&output));
        }
        let text = String::from_utf8(output.stdout).context("curl output is not UTF-8")?;
        let (status, body) = curl_status(&text)?;
        Ok(Response {
            status,
            headers: last_head(&String::from_utf8_lossy(&output.stderr)),
            body: body.to_owned(),
        })
    }

    /// Every item of a paginated list endpoint, read as `account`.
    pub(crate) async fn pages(&self, account: &Account, endpoint: &str) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        self.pages_while(account, endpoint, |batch| {
            items.extend(batch.iter().cloned());
            Ok(true)
        })
        .await?;
        Ok(items)
    }

    /// Hand each page of a paginated list endpoint, read as `account`, to
    /// `visit` until it returns false or no page follows.
    pub(crate) async fn pages_while(
        &self,
        account: &Account,
        endpoint: &str,
        mut visit: impl FnMut(&[Value]) -> Result<bool>,
    ) -> Result<()> {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let (size, page_size) = self.kind.page_size();
        for page in 1.. {
            let request = Request::get(format!(
                "{endpoint}{separator}{size}={page_size}&page={page}"
            ));
            let response = self.send(account, &request).await?;
            let (batch, more) = self
                .kind
                .page(&response, page_size)
                .with_context(|| format!("{endpoint} is not a list"))?;
            if !visit(batch)? || !more {
                break;
            }
        }
        Ok(())
    }
}

/// A page that is a JSON array; another follows while pages are full.
pub(crate) fn full_page(response: &Value, size: usize) -> Option<(&[Value], bool)> {
    let batch = response.as_array()?;
    Some((batch, batch.len() >= size))
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
pub(crate) fn json_body(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).context("invalid JSON response")
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

/// Why curl did not run to its end: a timeout may pass, a missing curl will
/// not.
fn curl_unrun(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return Transient::Interrupted(error.to_string()).into();
    }
    anyhow::Error::new(error)
        .context("curl is required for Forgejo, Gitea and Bitbucket API requests")
}

/// The failure curl's exit reports, marked when it may pass.
fn curl_failure(output: &std::process::Output) -> anyhow::Error {
    let message = format!(
        "curl failed ({}): {}",
        output.status,
        shoal_subprocess::diagnostic(&output.stderr).trim()
    );
    match Transient::from_curl(output.status.code(), message.clone()) {
        Some(transient) => transient.into(),
        None => anyhow::anyhow!(message),
    }
}

/// The body of a successful curl response, or the error a failed one reports.
pub(crate) fn curl_body(text: &str) -> Result<&str> {
    let (status, body) = curl_status(text)?;
    http_body(status, body)
}

/// The status of curl's output, which ends with the one `-w` appends on its
/// own line, and the body before it.
fn curl_status(text: &str) -> Result<(u16, &str)> {
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text));
    let status = status
        .trim()
        .parse()
        .context("response has no HTTP status")?;
    Ok((status, body))
}

/// The body of a successful response, or the error a failed one reports.
pub(crate) fn http_body(status: u16, body: &str) -> Result<&str> {
    if !(200..300).contains(&status) {
        let detail = error_message(body).unwrap_or_else(|| body.chars().take(500).collect());
        let message = match detail.trim() {
            "" => format!("HTTP {status}"),
            detail => format!("HTTP {status}: {detail}"),
        };
        match Transient::from_status(status, message.clone()) {
            Some(transient) => return Err(transient.into()),
            None => bail!(message),
        }
    }
    Ok(body)
}

/// A multipart form with `upload` as its one field, and its content type.
async fn multipart(upload: &Upload) -> Result<(String, Vec<u8>)> {
    let field = upload.field.context("a form upload names its field")?;
    let path = upload.path.clone();
    let content = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await?
        .with_context(|| format!("read {}", upload.path.display()))?;
    let name: String = upload
        .path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n'))
        .collect();
    let boundary = format!("shoal-{}", uuid::Uuid::new_v4().simple());
    let mut form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\n\
         Content-Type: {}\r\n\r\n",
        upload.content_type
    )
    .into_bytes();
    form.extend(content);
    form.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), form))
}

/// curl's arguments that send `upload`: a form field, whose quoted file name
/// escapes what curl reads specially, or the raw body.
fn upload_args(upload: &Upload) -> Result<Vec<String>> {
    let path = upload
        .path
        .to_str()
        .context("the attached file's path is not UTF-8")?;
    Ok(match upload.field {
        Some(field) => {
            let quoted = path.replace('\\', "\\\\").replace('"', "\\\"");
            vec![
                "-F".into(),
                format!("{field}=@\"{quoted}\";type={}", upload.content_type),
            ]
        }
        None => vec![
            "-H".into(),
            format!("Content-Type: {}", upload.content_type),
            "--data-binary".into(),
            format!("@{path}"),
        ],
    })
}

/// A curl config line, so the token never appears on a command line.
pub(crate) fn authorization(token: String) -> Vec<u8> {
    format!("header = \"Authorization: token {token}\"\n").into_bytes()
}

/// Where fj keeps the user's logins.
pub(crate) fn fj_keys_path() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    Ok(fj_keys_at(&home_dir()?, data))
}

/// Where fj run with `home` keeps its logins; `data` is its XDG data
/// directory, which Linux honors.
pub(crate) fn fj_keys_at(home: &Path, data: Option<PathBuf>) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/forgejo-cli.forgejo-cli/keys.json")
    } else {
        data.unwrap_or_else(|| home.join(".local/share"))
            .join("forgejo-cli/keys.json")
    }
}

/// The token fj saved for exactly this host.
pub(crate) fn fj_token(path: &Path, host: &str) -> Result<String> {
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

/// The user's home directory, where the CLIs keep their logins.
pub(crate) fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AgentLogins, Launch};

    /// Finds the CLIs only in one directory.
    struct Bin(PathBuf);

    impl Launch for Bin {
        fn apply(&self, command: &mut Command) {
            command.env("PATH", &self.0);
        }
    }

    #[tokio::test]
    async fn repeated_reads_send_the_validators_and_keep_the_unchanged_body() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        // Like gh, a 304 prints its head and exits with an error.
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
case "$*" in
  *"If-None-Match: \"v1\""*) printf 'HTTP/2.0 304 Not Modified\n\n'; exit 1;;
esac
printf 'HTTP/2.0 200 OK\nETag: "v1"\n\n{{"number":7}}'
"#,
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let agent = || {
            let logins = AgentLogins {
                launch: Box::new(Bin(dir.path().to_owned())),
                fj_home: None,
                fj: false,
                tea: false,
            };
            Account::agent(logins, &Default::default())
        };
        let memory = crate::transport::memory::Memory::default();
        let account = || agent().remembering(memory.clone());
        let repo = ForgeRepo::parse("https://github.com/team/project").unwrap();
        let read = Request::get("repos/team/project/issues/7".into());
        for _ in 0..2 {
            let body = repo.send(&account(), &read).await.unwrap();
            assert_eq!(body, serde_json::json!({"number": 7}));
        }
        let log = std::fs::read_to_string(log).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(!lines[0].contains("If-None-Match"), "{log}");
        assert!(lines[1].ends_with("-H If-None-Match: \"v1\""), "{log}");
        // A change is not conditional, and an account without the memory
        // reads in full.
        let edit = Request::new(HttpMethod::Patch, read.endpoint.clone(), None);
        repo.send(&account(), &edit).await.unwrap();
        repo.send(&agent(), &read).await.unwrap();
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(
            log.lines()
                .skip(2)
                .all(|line| !line.contains("If-None-Match")),
            "{log}"
        );
    }

    #[test]
    fn forgejo_responses_report_status_and_messages() {
        assert_eq!(curl_body("{\"number\":7}\n201").unwrap(), "{\"number\":7}");
        assert_eq!(curl_body("\n204").unwrap(), "");
        let error = curl_body("{\"message\":\"user does not exist\"}\n404").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 404: user does not exist");
        let error = curl_body("<html>busy</html>\n502").unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 502: <html>busy</html>");
        assert_eq!(format!("{:#}", curl_body("\n404").unwrap_err()), "HTTP 404");
        assert!(curl_body("{}").is_err());
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

    #[tokio::test]
    async fn forms_carry_the_file_under_its_field_and_a_plain_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a\"b.png");
        std::fs::write(&path, "png").unwrap();
        let upload = Upload {
            path,
            content_type: "image/png",
            field: Some("file"),
        };
        let (content_type, form) = multipart(&upload).await.unwrap();
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        assert_eq!(
            String::from_utf8(form).unwrap(),
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"ab.png\"\r\n\
                 Content-Type: image/png\r\n\r\npng\r\n--{boundary}--\r\n"
            )
        );
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
