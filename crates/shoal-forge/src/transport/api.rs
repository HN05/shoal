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
/// Bounds a response and the head the transport prints with it, unless
/// the request sets a lower one.
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

crate::state::states!(
    /// The HTTP methods forge requests use.
    HttpMethod: ValueEnum {
        Get => "GET",
        Post => "POST",
        Patch => "PATCH",
        Put => "PUT",
        Delete => "DELETE",
    }
);

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
    /// Environment variables a CLI would read a token from for any host,
    /// left out so the request carries only a login saved for its own host.
    pub withheld_tokens: &'static [&'static str],
    /// The most of the response, in bytes, a CLI may print for it.
    pub max_bytes: usize,
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
            withheld_tokens: &[],
            max_bytes: MAX_RESPONSE_BYTES,
        }
    }

    /// This request without the token variables `tokens`, for a host that
    /// may not be the forge they are meant for.
    pub fn withholding(mut self, tokens: &'static [&'static str]) -> Self {
        self.withheld_tokens = tokens;
        self
    }

    /// POST the file `upload` names, again if the forge did not get it.
    pub fn upload(endpoint: String, upload: Upload) -> Self {
        Self {
            upload: Some(upload),
            resend: Resend::Unreceived,
            ..Self::new(HttpMethod::Post, endpoint, None)
        }
    }

    /// Refuse a response longer than `bytes`.
    pub fn limited(mut self, bytes: usize) -> Self {
        self.max_bytes = bytes;
        self
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
        let recalled = read.then(|| memory.recall(&url)).flatten();
        let response = match &recalled {
            Some((conditions, _)) => {
                let conditional = request.conditional(conditions.clone());
                self.within_limit(account, &conditional).await?
            }
            None => self.within_limit(account, request).await?,
        };
        if let (304, Some((_, body))) = (response.status, recalled) {
            return Ok(body);
        }
        let body = json_body(response.success_body()?)?;
        if read {
            memory.remember(&url, &response, &body);
        }
        Ok(body)
    }

    /// The body of the read `request`, or the status of a refusal among
    /// `refusals`, which tells the caller something about the repository,
    /// such as an endpoint that does not apply to it. It is not remembered
    /// for a conditional read.
    pub(crate) async fn read_refusable(
        &self,
        account: &Account,
        request: &Request,
        refusals: &[u16],
    ) -> Result<Result<Value, u16>> {
        async {
            let response = self.within_limit(account, request).await?;
            if refusals.contains(&response.status) {
                return Ok(Err(response.status));
            }
            json_body(response.success_body()?).map(Ok)
        }
        .await
        .with_context(|| format!("{} {}", request.method.as_str(), request.endpoint))
    }

    /// The text of the response to `request`, which need not be JSON, such
    /// as a job log; it is not remembered for a conditional read.
    pub(crate) async fn read_text(&self, account: &Account, request: &Request) -> Result<String> {
        async {
            let response = self.within_limit(account, request).await?;
            response.success_body().map(str::to_owned)
        }
        .await
        .with_context(|| format!("{} {}", request.method.as_str(), request.endpoint))
    }

    /// The response to `request`, refused without sending it while the
    /// forge limits the account's requests.
    async fn within_limit(&self, account: &Account, request: &Request) -> Result<Response> {
        let memory = account.memory();
        memory.check_limit(&self.host, unix_seconds())?;
        let response = self.kind.send(self, account, request).await?;
        // Relative limits count from the response, which may have taken a while.
        memory.observe_limit(&self.host, &response, unix_seconds())?;
        Ok(response)
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
        for token in request.withheld_tokens {
            cli.env_remove(token);
        }
        let output = shoal_subprocess::Run::new(cli)
            .input(input)
            .timeout(request.timeout())
            .output_limit(request.max_bytes)
            .capture()
            .await
            .map_err(|error| {
                unrun(
                    error,
                    request.max_bytes,
                    format!("run {program}; is it installed?"),
                )
            })?;
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
        // -q first: a user .curlrc must not trace the token header. Without
        // globbing, braces and brackets in a path send one request, as given.
        curl.args(["-q", "-sS", "--globoff", "--max-time"])
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
            .output_limit(request.max_bytes)
            .capture()
            .await
            .map_err(|error| curl_unrun(error, request.max_bytes))?;
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

    /// The list an endpoint that ignores pages answers, read as `account`.
    pub(crate) async fn whole_list(&self, account: &Account, endpoint: &str) -> Result<Vec<Value>> {
        let response = self.send(account, &Request::get(endpoint.into())).await?;
        let (list, _) = (self.kind.page(&response, usize::MAX))
            .with_context(|| format!("{endpoint} is not a list"))?;
        Ok(list.to_vec())
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

/// Why curl did not run to its end: a timeout may pass, a missing curl or
/// a response over `limit` bytes will not.
fn curl_unrun(error: std::io::Error, limit: usize) -> anyhow::Error {
    match error.kind() {
        std::io::ErrorKind::TimedOut => Transient::Interrupted(error.to_string()).into(),
        std::io::ErrorKind::NotFound => {
            anyhow::Error::new(error).context("curl is required for this forge's API requests")
        }
        _ => unrun(error, limit, "read curl's response"),
    }
}

/// Why a program sending a request did not run to its end: a response over
/// `limit` bytes, which asking for fewer items avoids, or `failure`.
pub(crate) fn unrun(
    error: std::io::Error,
    limit: usize,
    failure: impl std::fmt::Display + Send + Sync + 'static,
) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::FileTooLarge {
        return anyhow::anyhow!(
            "the response is larger than {}; request fewer items, such as with a smaller page",
            size(limit)
        );
    }
    anyhow::Error::new(error).context(failure)
}

/// `bytes` in MiB when it is a whole number of them.
fn size(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
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
mod tests;
