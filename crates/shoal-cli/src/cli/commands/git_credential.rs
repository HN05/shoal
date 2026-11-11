//! The Git credential helper of an agent launch. Git runs it with the
//! operation as its argument and the request on standard input; for `get`
//! over HTTPS it answers with the login the daemon gives the caller's scope,
//! the agent token of the host. It answers nothing otherwise, or when the
//! daemon gives nothing, and Git moves on; `store` and `erase` do nothing.
//! When the daemon cannot answer, it tells Git to quit, so Git neither asks
//! for a password nor signs in as anyone else.
use std::io::{Read as _, Write as _};

use anyhow::Result;

use crate::cli::context::Context;

/// Answer Git's `operation` request on standard input.
pub(super) async fn run(ctx: &Context, operation: String) -> Result<i32> {
    if operation != "get" {
        return Ok(0);
    }
    let mut request = String::new();
    std::io::stdin().read_to_string(&mut request)?;
    let Some(host) = https_host(&request) else {
        return Ok(0);
    };
    let answer = match crate::client::agents::git_credential(&ctx.paths, host.to_owned()).await {
        Ok(Some(credential)) => format!(
            "username={}\npassword={}\n",
            credential.username, credential.password
        ),
        Ok(None) => String::new(),
        Err(error) => {
            eprintln!("shoal: no agent login for {host}: {error:#}");
            "quit=true\n".to_owned()
        }
    };
    std::io::stdout().lock().write_all(answer.as_bytes())?;
    Ok(0)
}

/// The host of a credential request over HTTPS, with its port when it
/// names one; none for any other protocol.
fn https_host(request: &str) -> Option<&str> {
    let attribute = |name: &str| {
        request
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };
    (attribute("protocol")? == "https")
        .then(|| attribute("host"))
        .flatten()
        .filter(|host| !host.is_empty())
}

#[cfg(test)]
mod tests;
