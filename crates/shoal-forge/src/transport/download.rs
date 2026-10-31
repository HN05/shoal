//! Files fetched from a forge's web host, such as an image in a description:
//! through a CLI's `api` command, which owns its login, or through curl with
//! a token passed on stdin. Both follow redirects to the storage that serves
//! the file and send the login only to the host it is for.
use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::{
    account::Account,
    transport::{
        api::{FILE_TIMEOUT, curl_failure, size},
        programs,
        response::{Fetched, last_head},
    },
};

/// The largest file Shoal downloads, which is also the largest it attaches.
pub(crate) const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
/// Room for the response heads printed with the file.
const HEAD_BYTES: usize = 64 * 1024;

/// GET `url` through `program api`, as `gh api` takes a full URL.
pub(crate) async fn through_cli(
    program: &'static str,
    account: &Account,
    url: &str,
) -> Result<Fetched> {
    let mut cli = Command::new(program);
    cli.env("NO_COLOR", "1")
        .env("GH_PROMPT_DISABLED", "1")
        .args(["api", "--include", url]);
    account.apply(&mut cli);
    let output = capture(cli, Vec::new())
        .await
        .map_err(|error| unrun(error, format!("run {program}; is it installed?")))?;
    // The CLIs exit with an error for a failed response, whose head says
    // more; after a successful head, an error means the file was cut off.
    let failed = !output.status.success();
    match Fetched::included(output.stdout) {
        Ok(fetched) if !failed || !(200..300).contains(&fetched.status) => Ok(fetched),
        Err(error) if !failed => Err(error),
        _ => bail!(
            "{program} failed: {}",
            shoal_subprocess::diagnostic(&output.stderr).trim()
        ),
    }
}

/// GET `url` with curl, passing the authorization header on stdin so no
/// command line shows it.
pub(crate) async fn with_curl(url: &str, authorization: Option<Vec<u8>>) -> Result<Fetched> {
    let mut curl = Command::new(programs::CURL);
    // -q first: a user .curlrc must not trace the token header.
    curl.args(["-q", "-sS", "--globoff", "--location", "--max-redirs", "5"])
        .args(["--proto-redir", "=http,https", "--max-time"])
        .arg(FILE_TIMEOUT.as_secs().to_string())
        // The heads go apart from the file, which ends with the status.
        .args(["-D", "/dev/stderr", "-w", "\n%{http_code}", "-K", "-", url]);
    let output = capture(curl, authorization.unwrap_or_default())
        .await
        .map_err(|error| unrun(error, "curl is required to download from this forge"))?;
    if !output.status.success() {
        return Err(curl_failure(&output));
    }
    let (body, status) = split_status(output.stdout)?;
    Ok(Fetched {
        status,
        headers: last_head(&String::from_utf8_lossy(&output.stderr)),
        body,
    })
}

/// The output of `command`, given `input`, within the download bounds.
async fn capture(command: Command, input: Vec<u8>) -> std::io::Result<std::process::Output> {
    shoal_subprocess::Run::new(command)
        .input(input)
        .timeout(FILE_TIMEOUT)
        .output_limit(MAX_DOWNLOAD_BYTES + HEAD_BYTES)
        .capture()
        .await
}

/// Why a program fetching a file did not run to its end: a file over the
/// download bound, or `failure`.
fn unrun(
    error: std::io::Error,
    failure: impl std::fmt::Display + Send + Sync + 'static,
) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::FileTooLarge {
        return anyhow::anyhow!("the file is larger than {}", size(MAX_DOWNLOAD_BYTES));
    }
    anyhow::Error::new(error).context(failure)
}

/// curl's output split into the file and the status `-w` appends on its
/// own line.
fn split_status(mut output: Vec<u8>) -> Result<(Vec<u8>, u16)> {
    let at = output
        .iter()
        .rposition(|&byte| byte == b'\n')
        .context("curl's output has no HTTP status")?;
    let status = std::str::from_utf8(&output[at + 1..])
        .ok()
        .and_then(|status| status.trim().parse().ok())
        .context("response has no HTTP status")?;
    output.truncate(at);
    Ok((output, status))
}

#[cfg(test)]
mod tests;
