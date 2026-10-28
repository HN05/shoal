//! Happy HTTP requests through curl, keeping the account token off argv.
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::process::Command;

use crate::tools::Tool;

const CLIENT_HEADER: &str = concat!("X-Happy-Client: shoal/", env!("CARGO_PKG_VERSION"));

/// A Happy server and the account token, spoken through `curl`.
pub(super) struct Server {
    url: String,
    token: String,
    /// Private directory for request bodies (`--data-binary @file`).
    temp_dir: PathBuf,
}

impl Server {
    pub fn new(url: String, token: String, temp_dir: PathBuf) -> Self {
        Self {
            url,
            token,
            temp_dir,
        }
    }

    pub async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut curl = Command::new(Tool::Curl.program());
        curl.args([
            "-sS",
            "-f",
            "--max-time",
            "30",
            "-X",
            method,
            "-H",
            "Content-Type: application/json",
            "-H",
            CLIENT_HEADER,
            "-K",
            "-",
        ]);
        let _body_file = match body {
            Some(body) => {
                let mut file = tempfile::NamedTempFile::new_in(&self.temp_dir)
                    .context("create request body file")?;
                serde_json::to_writer(&mut file, body)?;
                curl.arg("--data-binary");
                curl.arg(format!("@{}", file.path().display()));
                Some(file)
            }
            None => None,
        };
        curl.arg(format!("{}{path}", self.url));
        // curl's config syntax: the token never appears on a command line.
        let output = crate::subprocess::Run::new(curl)
            .input(format!("header = \"Authorization: Bearer {}\"\n", self.token).into_bytes())
            .timeout(Duration::from_secs(30))
            .checked()
            .await
            .with_context(|| {
                format!("Happy server request {method} {path} failed; curl is required")
            })?;
        if output.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("Happy server returned invalid JSON for {method} {path}"))
    }
}
