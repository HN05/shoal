//! The Shoal on another machine that a move reaches with `ssh`. The package
//! goes into `shoal --json restore --from -` there, so the other side stays
//! a plain command.
use std::{path::Path, process::Stdio};

use anyhow::{Context as _, Result, ensure};

use crate::client::output::{
    error::{CommandError, ErrorDetail},
    restore::Reopened,
};
use crate::protocol::ErrorCode;
use shoal_core::transfer::{Arrived, SshDestination};

/// The exit status of a remote shell that cannot find the command.
const NOT_FOUND: i32 = 127;
/// The exit status of `ssh` when it cannot connect or authenticate.
const SSH_FAILED: i32 = 255;

/// The Shoal on another machine, which `ssh` reaches.
pub(super) struct Remote<'a> {
    destination: &'a SshDestination,
}

/// Why an import on the other machine failed.
pub(super) enum ImportFailure {
    /// `ssh` could not run, or Shoal there failed before it recorded
    /// anything, as its `not_imported` error says, so that machine has
    /// nothing of the space.
    Refused(anyhow::Error),
    /// It may have recorded the space there, as when setup there failed
    /// after the import, the connection failed or dropped or its report
    /// cannot be read.
    Unknown(anyhow::Error),
}

impl<'a> Remote<'a> {
    pub(super) fn new(destination: &'a SshDestination) -> Self {
        Self { destination }
    }

    pub(super) fn host(&self) -> &str {
        &self.destination.host
    }

    /// Refuse a machine `ssh` cannot reach without a prompt, or whose
    /// Shoal does not answer.
    pub(super) async fn check(&self) -> Result<()> {
        let output = self
            .command(&["repo", "ls"])
            .stdin(Stdio::null())
            .output()
            .await
            .context("run ssh")?;
        let hint = match output.status.code() {
            Some(NOT_FOUND) => "; --shoal names the shoal program there when ssh's PATH lacks it",
            Some(SSH_FAILED) => {
                "; the daemon runs the move, so ssh must reach the host without a prompt, with a key that works without one, as Git's fetches need"
            }
            _ => "",
        };
        ensure!(
            output.status.success(),
            "cannot reach Shoal on {} with ssh, so nothing was removed: {}{hint}",
            self.host(),
            error_message(&output)
        );
        Ok(())
    }

    /// Import the package in `file` there, queueing its agents' restores
    /// unless it leaves them stopped, and return where it arrived.
    pub(super) async fn import(&self, file: &Path) -> Result<Arrived, ImportFailure> {
        let mut args = vec!["restore", "--from", "-"];
        if !self.destination.resume {
            args.push("--no-resume");
        }
        let input =
            std::fs::File::open(file).map_err(|error| ImportFailure::Refused(error.into()))?;
        let output = self
            .command(&args)
            .stdin(input)
            .output()
            .await
            .context("run ssh")
            .map_err(ImportFailure::Refused)?;
        if !output.status.success() {
            let failed = format!(
                "the import on {} failed: {}",
                self.host(),
                error_message(&output)
            );
            let nothing_recorded = reported_error(&output)
                .is_some_and(|error| error.code == ErrorCode::NotImported.to_string());
            return Err(match nothing_recorded {
                true => ImportFailure::Refused(anyhow::anyhow!(failed)),
                false => ImportFailure::Unknown(anyhow::anyhow!(failed)),
            });
        }
        let imported: Reopened = serde_json::from_slice(&output.stdout)
            .context("read the import's report")
            .map_err(ImportFailure::Unknown)?;
        Ok(Arrived {
            space: imported.space,
            queued: (imported.queued.map(|queued| queued.queued)).unwrap_or_default(),
        })
    }

    /// `ssh` running shoal with `args` and JSON output there, never
    /// prompting, quoted for the remote shell, which ssh hands the joined
    /// command, with its output captured.
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let shoal = self.destination.shoal.as_deref().unwrap_or("shoal");
        let mut words = vec![shoal, "--json"];
        words.extend(args);
        let mut command = tokio::process::Command::new("ssh");
        command
            .args(["-o", "BatchMode=yes", "--"])
            .arg(self.host())
            .arg(crate::shell::quote(&words))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

impl From<ImportFailure> for anyhow::Error {
    fn from(failure: ImportFailure) -> Self {
        match failure {
            ImportFailure::Refused(error) | ImportFailure::Unknown(error) => error,
        }
    }
}

/// The JSON error Shoal wrote to standard error when the remote command
/// failed there; none when ssh or the remote shell failed.
fn reported_error(output: &std::process::Output) -> Option<ErrorDetail> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().rev().find_map(|line| {
        let failed: CommandError = serde_json::from_str(line).ok()?;
        Some(failed.error)
    })
}

/// The error a failed remote command reported: the message of the JSON error
/// Shoal writes to standard error, or what ssh or the remote shell wrote.
fn error_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    match reported_error(output) {
        Some(error) => error.message.trim().to_owned(),
        None if stderr.trim().is_empty() => output.status.to_string(),
        None => stderr.trim().to_owned(),
    }
}
