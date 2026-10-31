//! Files fetched from a forge's web host, such as an image in a description,
//! through curl with the account's token passed on stdin. It follows
//! redirects to the storage that serves the file and sends the token only
//! to the host it is for.
use anyhow::Result;
use tokio::process::Command;

use crate::transport::{
    api::{FILE_TIMEOUT, curl_failure, size},
    programs,
    response::{Fetched, WRITE_OUT, split_output},
};

/// The largest file Shoal downloads, which is also the largest it attaches.
pub(crate) const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
/// Room for the response heads printed with the file.
const HEAD_BYTES: usize = 64 * 1024;

/// GET `url` with curl, passing the authorization header on stdin so no
/// command line shows it.
pub(crate) async fn with_curl(url: &str, authorization: Option<Vec<u8>>) -> Result<Fetched> {
    let mut curl = Command::new(programs::CURL);
    // -q first: a user .curlrc must not trace the token header.
    curl.args(["-q", "-sS", "--globoff", "--location", "--max-redirs", "5"])
        .args(["--proto-redir", "=http,https", "--max-time"])
        .arg(FILE_TIMEOUT.as_secs().to_string())
        // The heads go before the file, as for an API request.
        .args(["-D", "-", "-w", WRITE_OUT, "-K", "-", url]);
    let output = capture(curl, authorization.unwrap_or_default())
        .await
        .map_err(|error| unrun(error, "curl is required to download from this forge"))?;
    if !output.status.success() {
        return Err(curl_failure(&output));
    }
    let split = split_output(&output.stdout)?;
    let mut body = output.stdout;
    body.truncate(split.body.end);
    body.drain(..split.body.start);
    Ok(Fetched {
        status: split.status,
        headers: split.headers,
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
