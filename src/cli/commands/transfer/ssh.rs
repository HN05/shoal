//! `shoal move ssh`: hand workspaces to another machine's Shoal, as
//! `shoal move export | ssh <host> shoal move import --resume` does, once
//! `ssh` reaches Shoal there.
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

use super::export::{self, ExportDirectory, PlannedExport};
use crate::{
    cli::{
        args::transfer::SshArgs,
        client,
        context::Context,
        ui::{self, Fallback},
    },
    model::Workspace,
    subprocess::{Run, diagnostic},
};

/// The exit status of a remote shell that cannot find the command.
const NOT_FOUND: i32 = 127;

pub(super) async fn run(ctx: &Context, args: SshArgs) -> Result<i32> {
    export::ensure_unscoped_export(ctx)?;
    let remote = Remote {
        host: args.host,
        shoal: args.shoal,
    };
    let workspaces = match args.repository {
        Some(repository) => export::repository_workspaces(ctx, repository).await?,
        None => selected(ctx, args.workspace).await?,
    };
    ctx.progress(
        &format!("Reaching Shoal on {}", remote.host),
        remote.check(),
    )
    .await?;
    let staging = tempfile::tempdir()?;
    let directory = ExportDirectory::create(staging.path().to_owned())?;
    let heading = match workspaces.as_slice() {
        [workspace] => format!("Move {} to {}", workspace.name, remote.host),
        _ => format!("Move {} workspaces to {}", workspaces.len(), remote.host),
    };
    let operation = ("move", "move");
    let yes = args.confirmation.yes;
    let planned =
        export::check_exports(ctx, &workspaces, &directory, yes, &heading, operation).await?;
    let mut report = ui::Report::new("moved", "move");
    for planned in planned {
        let name = planned.name().to_owned();
        match move_one(ctx, &remote, planned, !args.no_resume).await {
            Ok(imported) => {
                let message = moved(&name, &remote.host, &imported);
                report.done(ctx, &message, imported)
            }
            Err(error) => report.failed(ctx, &name, &error),
        }
    }
    let (moved, failed) = report.counts();
    let summary = match (moved, failed) {
        (1, 0) => String::new(),
        (_, 0) => format!("Moved {moved} workspaces to {}", remote.host),
        _ => format!(
            "Moved {moved} of {} workspaces to {}; {failed} failed",
            moved + failed,
            remote.host
        ),
    };
    report.finish(ctx, &summary)
}

/// The report of moving `name` to `host`, where the import reported
/// `imported`.
fn moved(name: &str, host: &str, imported: &Value) -> String {
    let mut message = format!("Moved {name} to {host}");
    if let Some(path) = imported["workspace"]["path"].as_str() {
        message.push_str(&format!(" at {path}"));
    }
    let queued = imported["queued"]["queued"].as_array().map_or(0, Vec::len);
    match queued {
        0 => {}
        1 => message.push_str("; its agent continues there"),
        _ => message.push_str("; its agents continue there"),
    }
    message
}

/// The workspaces `names` selects, or the current one or one the picker
/// offers without names.
async fn selected(ctx: &Context, names: Vec<String>) -> Result<Vec<Workspace>> {
    let selectors = match names.len() {
        0 | 1 => {
            let named = names.into_iter().next();
            vec![ui::select_workspace(ctx, named, Fallback::CurrentDirectory).await?]
        }
        _ => ui::select_workspaces(ctx, names).await?,
    };
    let mut workspaces = Vec::new();
    for selector in selectors {
        workspaces.push(client::inspect(&ctx.paths, selector).await?.workspace);
    }
    Ok(workspaces)
}

/// Remove and export the planned workspace, then import it on `remote`,
/// queueing its agents' restores there when `resume`.
async fn move_one(
    ctx: &Context,
    remote: &Remote,
    planned: PlannedExport,
    resume: bool,
) -> Result<Value> {
    let name = planned.name().to_owned();
    let file = planned.export(ctx).await?;
    remote.import(&file, resume).await.with_context(|| {
        format!(
            "{name} is removed here; shoal work restore {name} opens it here again, unless {} opened it",
            remote.host
        )
    })
}

/// The Shoal on another machine, which `ssh` reaches.
struct Remote {
    host: String,
    /// The shoal program there.
    shoal: String,
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
    /// `resume`, and return what the import reported.
    async fn import(&self, file: &Path, resume: bool) -> Result<Value> {
        let mut args = vec!["move", "import"];
        if resume {
            args.push("--resume");
        }
        let input = std::fs::File::open(file)?;
        let output = Run::new(self.command(&args))
            .input_file(input)
            .capture()
            .await
            .context("run ssh")?;
        if !output.status.success() {
            bail!(
                "the import on {} failed: {}",
                self.host,
                error_message(&output)
            );
        }
        serde_json::from_slice(&output.stdout).context("read the import's report")
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

/// The error a failed remote command reported: the message of the JSON error
/// Shoal writes to standard error, or what ssh or the remote shell wrote.
fn error_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reported = stderr.lines().rev().find_map(|line| {
        let error: Value = serde_json::from_str(line).ok()?;
        error["error"]["message"].as_str().map(str::to_owned)
    });
    match reported {
        Some(message) => diagnostic(message.as_bytes()),
        None if stderr.trim().is_empty() => output.status.to_string(),
        None => diagnostic(stderr.trim().as_bytes()),
    }
}
