//! `shoal repo move`: move every repository directory into a new `root_dir`.
use super::super::resume::queue::{restore_stopped, saved_executions};
use crate::cli::commands::resume::reports::QueuedExt;
use crate::client::output::resume::Queued;
use crate::config_file::ConfigExt;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transfer::StoppedExecutions;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::repository::MoveArgs,
        client,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    config::{Config, edit::Change},
    model::{Execution, MovedRepository, RepositoryMoves, Spaces},
    protocol::{QueuedLaunches, UntrackedProcesses},
};

/// Check that every repository outside the root `args` name, or the
/// configured root, can move, with the files of the chats without a
/// repository, confirm, save that root as `root_dir` when given, stop the
/// commands and agents running in them, then have the daemon move them,
/// into a directory Spotlight skips unless they ask for a regular one, and
/// queue the stopped agents' restores, even when the move fails.
pub(super) async fn run(ctx: &Context, args: MoveArgs) -> Result<()> {
    let MoveArgs {
        root,
        regular_dir,
        confirmation,
    } = args;
    let confirmed = confirmation.yes;
    let root = root.map(|root| super::absolute(ctx, root)).transpose()?;
    let moves = plan_moves(ctx, root.clone(), regular_dir).await?;
    if !confirmed && !(moves.repositories.is_empty() && moves.chats.is_empty()) {
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        ensure!(
            ui::confirm(ctx, &plan(&moves, &spaces), "--yes")?,
            "repository move canceled"
        );
    }
    if let Some(root) = root {
        save_root(ctx, &root).await?;
    }
    let mut stopped = StoppedExecutions::new();
    let moved = async {
        stop_running(ctx, &moves.executions, confirmed, &mut stopped).await?;
        move_repositories(ctx, regular_dir).await
    }
    .await;
    let queued = restore_stopped(ctx, &stopped).await;
    show(ctx, &moved?, queued.as_ref())
}

async fn plan_moves(
    ctx: &Context,
    root: Option<PathBuf>,
    regular_dir: bool,
) -> Result<RepositoryMoves> {
    ctx.progress(
        "Checking repositories",
        crate::client::repositories::plan_moves(&ctx.paths, root, regular_dir),
    )
    .await
}

/// Stop the spaces `executions` run in, keeping their agents' sessions, and
/// add to `stopped` the executions each stop saved for `shoal resume`, by
/// space, including any started since the plan or ended by a stop that
/// failed. A CI job's workspace is waited for until the job, whose run
/// the stop cancelled, ends and its workspace goes, and stays out of
/// `stopped`. An execution Shoal cannot prove ended stops once the user
/// confirms it did, or when `confirmed`.
async fn stop_running(
    ctx: &Context,
    executions: &[Execution],
    confirmed: bool,
    stopped: &mut StoppedExecutions,
) -> Result<()> {
    let workspaces: BTreeSet<_> = executions.iter().map(|e| &e.space_id).collect();
    for workspace in workspaces {
        // A CI job ends with the other jobs of its run, which a stop before
        // cancelled, and its workspace goes with it.
        if !space_listed(ctx, workspace).await? {
            continue;
        }
        let saved_before = saved_executions(ctx, workspace).await?;
        let result = ctx
            .progress(
                "Stopping commands and agents",
                super::super::stop::stop(
                    ctx,
                    &SpaceSelector::id(workspace),
                    confirmed,
                    QueuedLaunches::Keep,
                    UntrackedProcesses::Keep,
                ),
            )
            .await;
        // A CI job's workspace goes with the job, leaving nothing to resume.
        if let Ok(outcome) = &result
            && outcome.cancelled_run.is_some()
        {
            ctx.progress(
                "Waiting for a cancelled CI job to end",
                job_ended(ctx, workspace),
            )
            .await?;
            continue;
        }
        let saved = saved_executions(ctx, workspace).await?;
        stopped.insert(
            workspace.clone(),
            saved.difference(&saved_before).cloned().collect(),
        );
        result?;
    }
    Ok(())
}

/// Whether the daemon still lists the space `id`.
async fn space_listed(ctx: &Context, id: &SpaceId) -> Result<bool> {
    let selector = SpaceSelector::id(id);
    Ok((crate::client::spaces::present_space(&ctx.paths, &selector).await?).is_some())
}

/// How long a move waits for a cancelled CI job's runner to stop it.
const JOB_END: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Wait until the workspace of a CI job whose run was cancelled is gone,
/// which the daemon removes once the forge's runner stops the job.
async fn job_ended(ctx: &Context, workspace: &SpaceId) -> Result<()> {
    let deadline = tokio::time::Instant::now() + JOB_END;
    loop {
        if !space_listed(ctx, workspace).await? {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the cancelled CI job in workspace {workspace} still runs; move the root once the forge has stopped it"
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn move_repositories(ctx: &Context, regular_dir: bool) -> Result<Vec<MovedRepository>> {
    ctx.progress(
        "Moving repositories",
        crate::client::repositories::move_all(&ctx.paths, regular_dir),
    )
    .await
}

fn show(ctx: &Context, moved: &Vec<MovedRepository>, queued: Option<&Queued>) -> Result<()> {
    ctx.show(moved, |moved| {
        let palette = Palette::stdout(ctx.json);
        if moved.is_empty() {
            outln!("No repository directories to move");
        }
        for moved in moved {
            outln!(
                "{}",
                palette.paint(
                    Style::Success,
                    format_args!(
                        "Moved {} to {}",
                        moved.repository.display_name(),
                        moved.repository.workspaces_dir.display()
                    )
                )
            );
        }
        if let Some(summary) = queued.and_then(Queued::summary) {
            outln!("{summary}");
        }
    })
}

fn plan(moves: &RepositoryMoves, spaces: &Spaces) -> String {
    let mut text = format!(
        "Move repository directories into {}:",
        moves.directory.display()
    );
    for repo in &moves.repositories {
        text.push_str(&format!(
            "\n  {}: {}",
            repo.display_name(),
            repo.workspaces_dir.display()
        ));
    }
    if !moves.chats.is_empty() {
        text.push_str("\nMove the files of these chats without a repository:");
    }
    for chat in &moves.chats {
        text.push_str(&format!("\n  {chat}"));
    }
    if !moves.executions.is_empty() {
        text.push_str("\nStop, then restore after the move:");
    }
    for execution in &moves.executions {
        let space = spaces.find(&SpaceSelector::id(&execution.space_id));
        let workspace = (space.as_ref()).map_or(execution.space_id.as_str(), |space| &space.name);
        let what = execution
            .agent
            .as_ref()
            .map(|agent| agent.name())
            .unwrap_or("command");
        text.push_str(&format!("\n  {workspace}: {what}"));
    }
    text
}

/// Save `root` as the global `root_dir` and apply it to the daemon, which
/// moves repositories into the root it has loaded.
async fn save_root(ctx: &Context, root: &Path) -> Result<()> {
    let value = toml::Value::String(
        root.to_str()
            .ok_or_else(|| anyhow::anyhow!("root directory is not UTF-8"))?
            .to_owned(),
    );
    Config::edit(
        &ctx.paths,
        &[Change {
            key: "machine.root_dir".into(),
            value: Some(value.to_string()),
        }],
    )?;
    client::reload_config(&ctx.paths).await?;
    Ok(())
}
