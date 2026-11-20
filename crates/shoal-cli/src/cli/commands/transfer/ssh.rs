//! `shoal move --ssh`: hand spaces to another machine's Shoal, ending them
//! here once imported there, or copies of them with `--keep`, once `ssh`
//! reaches Shoal there.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

mod copy;
mod moving;

use super::{Moving, export};
use crate::client::output::{
    error::{CommandError, ErrorDetail},
    restore::Reopened,
};
use crate::protocol::ErrorCode;
use crate::{
    cli::context::Context,
    subprocess::{Run, diagnostic},
};

/// The exit status of a remote shell that cannot find the command.
const NOT_FOUND: i32 = 127;

/// Move the spaces `moving` takes to `remote`, or copies of them, once
/// Shoal answers there.
pub(super) async fn run(ctx: &Context, remote: Remote, moving: Moving) -> Result<i32> {
    let spaces = match moving.repository {
        Some(repository) => export::repository_spaces(ctx, repository).await?,
        None => export::selected_spaces(ctx, moving.names).await?,
    };
    if moving.keep {
        export::copy::ensure_one_at_checkpoint(spaces.len(), moving.checkpoint)?;
    }
    ctx.progress(
        &format!("Reaching Shoal on {}", remote.host),
        remote.check(),
    )
    .await?;
    let carrying = export::carrying(moving.recursive, &spaces);
    match moving.keep {
        true => copy::copy_spaces(ctx, &remote, &spaces, moving.checkpoint, &carrying).await,
        false => moving::move_spaces(ctx, &remote, &spaces, moving.yes, &carrying).await,
    }
}

/// The summary of a batch that `verb` names, such as `Moved`, to `host`,
/// which acted on `done` spaces and failed on `failed`; none for one space
/// whose own line says it all.
fn summary(verb: &str, host: &str, (done, failed): (usize, usize)) -> String {
    match (done, failed) {
        (1, 0) => String::new(),
        (_, 0) => format!("{verb} {done} spaces to {host}"),
        _ => format!(
            "{verb} {done} of {} spaces to {host}; {failed} failed",
            done + failed
        ),
    }
}

/// Where a space went on `host`, whose import reported `imported`: a
/// workspace's path there, and whether its agents continue there.
fn arrived(host: &str, imported: &Reopened) -> String {
    let mut message = host.to_owned();
    if let Some(workspace) = imported.space.worktree() {
        message.push_str(&format!(" at {}", workspace.path.display()));
    }
    let queued = (imported.queued.as_ref()).map_or(0, |queued| queued.queued.len());
    match queued {
        0 => {}
        1 => message.push_str("; its agent continues there"),
        _ => message.push_str("; its agents continue there"),
    }
    message
}

/// The Shoal on another machine, which `ssh` reaches.
pub(super) struct Remote {
    host: String,
    /// The shoal program there.
    shoal: String,
    /// Whether imports there resume the agents.
    resume: bool,
}

impl Remote {
    /// The Shoal `shoal`, `shoal` on SSH's PATH by default, that `ssh`
    /// reaches at `host`, whose imports resume the agents when `resume`.
    pub(super) fn new(host: String, shoal: Option<String>, resume: bool) -> Self {
        Self {
            host,
            shoal: shoal.unwrap_or_else(|| "shoal".to_owned()),
            resume,
        }
    }

    /// Refuse a machine `ssh` cannot reach, or whose Shoal does not answer.
    async fn check(&self) -> Result<()> {
        let output = Run::new(self.command(&["repo", "ls"]))
            .capture()
            .await
            .context("run ssh")?;
        let hint = match output.status.code() {
            Some(NOT_FOUND) => "; --shoal names the shoal program there when ssh's PATH lacks it",
            _ => "",
        };
        ensure!(
            output.status.success(),
            "cannot reach Shoal on {} with ssh, so nothing was removed: {}{hint}",
            self.host,
            error_message(&output)
        );
        Ok(())
    }

    /// Import the export in `file` there, queueing its agents' restores
    /// unless it leaves them stopped, and return what the import reported.
    async fn import(&self, file: &Path) -> Result<Reopened, ImportFailure> {
        let mut args = vec!["restore", "--from", "-"];
        if !self.resume {
            args.push("--no-resume");
        }
        let input =
            std::fs::File::open(file).map_err(|error| ImportFailure::Refused(error.into()))?;
        let output = Run::new(self.command(&args))
            .input_file(input)
            .capture()
            .await
            .context("run ssh")
            .map_err(ImportFailure::Refused)?;
        if !output.status.success() {
            let failed = format!(
                "the import on {} failed: {}",
                self.host,
                error_message(&output)
            );
            let nothing_recorded = reported_error(&output)
                .is_some_and(|error| error.code == ErrorCode::NotImported.to_string());
            return Err(match nothing_recorded {
                true => ImportFailure::Refused(anyhow::anyhow!(failed)),
                false => ImportFailure::Unknown(anyhow::anyhow!(failed)),
            });
        }
        serde_json::from_slice(&output.stdout)
            .context("read the import's report")
            .map_err(ImportFailure::Unknown)
    }

    /// `ssh` running shoal with `args` and JSON output there, quoted for the
    /// remote shell, which ssh hands the joined command.
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut words = vec![self.shoal.as_str(), "--json"];
        words.extend(args);
        let mut command = tokio::process::Command::new("ssh");
        command
            .arg("--")
            .arg(&self.host)
            .arg(crate::shell::quote(&words));
        command
    }
}

/// Why an import on the other machine failed.
enum ImportFailure {
    /// `ssh` could not run, or Shoal there failed before it recorded
    /// anything, as its `not_imported` error says, so that machine has
    /// nothing of the space.
    Refused(anyhow::Error),
    /// It may have recorded the space there, as when setup there failed
    /// after the import, the connection failed or dropped or its report
    /// cannot be read.
    Unknown(anyhow::Error),
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
        Some(error) => diagnostic(error.message.as_bytes()),
        None if stderr.trim().is_empty() => output.status.to_string(),
        None => diagnostic(stderr.trim().as_bytes()),
    }
}
