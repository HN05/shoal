//! `shoal move ssh`: hand spaces to another machine's Shoal, ending them
//! here once imported there, or copies of them with `--keep`, once `ssh`
//! reaches Shoal there.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

mod copy;
mod moving;

use super::export;
use crate::client::output::transfer::Imported;
use crate::{
    cli::{args::transfer::SshArgs, context::Context},
    subprocess::{Run, diagnostic},
};

/// The exit status of a remote shell that cannot find the command.
const NOT_FOUND: i32 = 127;

pub(super) async fn run(ctx: &Context, args: SshArgs) -> Result<i32> {
    export::ensure_unscoped_export(ctx)?;
    let remote = Remote {
        host: args.host,
        shoal: args.shoal,
        restore: !args.no_restore,
    };
    let spaces = match args.repository {
        Some(repository) => export::repository_spaces(ctx, repository).await?,
        None => export::selected_spaces(ctx, args.space).await?,
    };
    if args.keep {
        export::copy::ensure_one_at_checkpoint(spaces.len(), args.checkpoint)?;
    }
    ctx.progress(
        &format!("Reaching Shoal on {}", remote.host),
        remote.check(),
    )
    .await?;
    match args.keep {
        true => copy::copy_spaces(ctx, &remote, &spaces, args.checkpoint).await,
        false => moving::move_spaces(ctx, &remote, &spaces, args.confirmation.yes).await,
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
/// workspace, or a chat started in the background there.
fn arrived(host: &str, imported: &Imported) -> String {
    let mut message = host.to_owned();
    let queued = match imported {
        Imported::Workspace(reopened) => {
            message.push_str(&format!(" at {}", reopened.workspace.path.display()));
            reopened
                .queued
                .as_ref()
                .map_or(0, |queued| queued.queued.len())
        }
        Imported::Chat(chat) => {
            message.push_str(&format!(". On {host}: {}", chat.place));
            0
        }
    };
    match queued {
        0 => {}
        1 => message.push_str("; its agent continues there"),
        _ => message.push_str("; its agents continue there"),
    }
    message
}

/// The Shoal on another machine, which `ssh` reaches.
struct Remote {
    host: String,
    /// The shoal program there.
    shoal: String,
    /// Whether imports there queue the agents' restores and start the
    /// chats they bring.
    restore: bool,
}

impl Remote {
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

    /// Import the export in `file` there, queueing its agents' restores when
    /// it resumes them, and return what the import reported.
    async fn import(&self, file: &Path) -> Result<Imported, ImportFailure> {
        let mut args = vec!["move", "import"];
        if self.restore {
            args.push("--restore");
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
            return Err(match reported_error(&output) {
                Some(_) => ImportFailure::Refused(anyhow::anyhow!(failed)),
                None => ImportFailure::Unknown(anyhow::anyhow!(failed)),
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
    /// Shoal there refused it, or it never reached Shoal there, which then
    /// has nothing of the space.
    Refused(anyhow::Error),
    /// It ran, but how it ended is unknown, as when the connection dropped
    /// or its report cannot be read, so the other machine may have the space.
    Unknown(anyhow::Error),
}

impl From<ImportFailure> for anyhow::Error {
    fn from(failure: ImportFailure) -> Self {
        match failure {
            ImportFailure::Refused(error) | ImportFailure::Unknown(error) => error,
        }
    }
}

/// The message of the JSON error Shoal wrote to standard error when the
/// remote command failed there; none when ssh or the remote shell failed.
fn reported_error(output: &std::process::Output) -> Option<String> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().rev().find_map(|line| {
        let error: Value = serde_json::from_str(line).ok()?;
        error["error"]["message"].as_str().map(str::to_owned)
    })
}

/// The error a failed remote command reported: the message of the JSON error
/// Shoal writes to standard error, or what ssh or the remote shell wrote.
fn error_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reported = reported_error(output);
    match reported {
        Some(message) => diagnostic(message.as_bytes()),
        None if stderr.trim().is_empty() => output.status.to_string(),
        None => diagnostic(stderr.trim().as_bytes()),
    }
}
