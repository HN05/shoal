//! Gitea's CLI, `tea`, as a Forgejo login: its saved login for a host sends
//! the request through `tea api`, so Shoal never reads tea's token.
use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;

use crate::{
    account::Account,
    transport::{
        api::{Request, unrun},
        programs,
        response::{Response, last_head},
    },
};

/// A tea login and the root URL of the forge it signs in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeaLogin {
    pub name: String,
    pub url: String,
}

/// The tea login, as `account` runs tea, for the forge at `host`.
pub(crate) async fn login(account: &Account, host: &str) -> Result<Option<TeaLogin>> {
    let mut tea = Command::new(programs::TEA);
    tea.env("NO_COLOR", "1")
        .args(["login", "list", "--output", "json"]);
    account.apply(&mut tea);
    let output = shoal_subprocess::Run::new(tea)
        .timeout(crate::transport::api::TIMEOUT)
        .output()
        .await?;
    login_for(&output, host)
}

/// Send `request` as `login` and return the body of a successful response.
pub(crate) async fn send(
    account: &Account,
    login: &TeaLogin,
    request: &Request,
    limit: usize,
) -> Result<String> {
    let response = exchange(account, login, request, limit).await?;
    response.success_body().map(str::to_owned)
}

/// Send `request` as `login` and return the forge's response.
pub(crate) async fn exchange(
    account: &Account,
    login: &TeaLogin,
    request: &Request,
    limit: usize,
) -> Result<Response> {
    ensure!(
        request.upload.is_none(),
        "tea cannot upload files; save an fj login for the host with fj auth login"
    );
    let url = format!("{}/api/v1/{}", login.url, request.endpoint);
    let mut tea = Command::new(programs::TEA);
    tea.env("NO_COLOR", "1").args([
        "api",
        "--include",
        "--login",
        &login.name,
        "--method",
        request.method.as_str(),
    ]);
    if request.body.is_some() {
        tea.args(["--data", "@-"]);
    }
    tea.arg(&url);
    account.apply(&mut tea);
    let body = request.body.as_ref().map(ToString::to_string);
    let output = shoal_subprocess::Run::new(tea)
        .input(body.unwrap_or_default())
        .timeout(crate::transport::api::TIMEOUT)
        .output_limit(limit)
        .capture()
        .await
        .map_err(|error| unrun(error, limit, "run tea; is it installed?"))?;
    ensure!(
        output.status.success(),
        "tea failed: {}",
        shoal_subprocess::diagnostic(&output.stderr).trim()
    );
    // tea reports HTTP failures with exit status zero; --include adds the head.
    let head = String::from_utf8_lossy(&output.stderr);
    Ok(Response {
        status: status(&head)?,
        headers: last_head(&head),
        body: String::from_utf8(output.stdout).context("tea output is not UTF-8")?,
    })
}

/// The login whose URL names `host`; an exact host and port wins over a
/// login on another port of the same host, which an SSH remote cannot name.
fn login_for(text: &str, host: &str) -> Result<Option<TeaLogin>> {
    #[derive(serde::Deserialize)]
    struct Login {
        name: String,
        url: String,
    }
    let logins: Vec<Login> = serde_json::from_str(text).context("invalid tea login list")?;
    let logins: Vec<(TeaLogin, String)> = logins
        .into_iter()
        .filter_map(|login| {
            let url = login.url.trim_end_matches('/');
            let (_, authority) = url.split_once("://")?;
            // A forge served below a path cannot have this repository's URLs.
            (!authority.contains('/')).then(|| {
                (
                    TeaLogin {
                        name: login.name,
                        url: url.to_owned(),
                    },
                    authority.to_ascii_lowercase(),
                )
            })
        })
        .collect();
    let exact = logins.iter().find(|(_, authority)| authority == host);
    let same_host = || {
        logins
            .iter()
            .find(|(_, authority)| !host.contains(':') && authority.split(':').next() == Some(host))
    };
    Ok(exact.or_else(same_host).map(|(login, _)| login.clone()))
}

/// The status line `tea api --include` writes to standard error.
fn status(stderr: &str) -> Result<u16> {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("HTTP/") {
            let status = rest.split_whitespace().nth(1).unwrap_or_default();
            return status
                .parse()
                .context("tea reported an invalid HTTP status");
        }
    }
    bail!("tea did not report an HTTP status")
}

#[cfg(test)]
mod tests;
