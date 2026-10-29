//! Move every repository directory into a new `root_dir`.
use super::resume::Queued;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use crate::{
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    config::{Config, edit::Change},
    model::{Execution, MovedRepository, Repository, RepositoryMoves, Workspace},
    protocol::RepositoryMethod,
    repository,
};

/// Check that every repository outside `root`, or the configured root, can
/// move, confirm, save `root` as `root_dir` when given, stop the commands and
/// agents running in them, then have the daemon move them, into a directory
/// Spotlight skips unless `regular_dir`, and queue the stopped agents'
/// restores, even when the move fails.
pub(super) async fn run(
    ctx: &Context,
    root: Option<PathBuf>,
    regular_dir: bool,
    confirmed: bool,
) -> Result<()> {
    let root = root
        .map(|root| super::repositories::absolute(ctx, root))
        .transpose()?;
    let before = Registrations::load(ctx).await?;
    let moves = plan_moves(ctx, root.clone(), regular_dir).await?;
    if !confirmed && !moves.repositories.is_empty() {
        ensure!(
            ui::confirm(ctx, &plan(&moves, &before.workspaces), "--yes")?,
            "repository move canceled"
        );
    }
    if let Some(root) = root {
        save_root(ctx, &root).await?;
    }
    let mut stopped = Vec::new();
    let moved = async {
        stop_running(ctx, &moves.executions, confirmed, &mut stopped).await?;
        move_repositories(ctx, regular_dir).await
    }
    .await;
    // A failed request may follow moves that completed.
    retrust(ctx, &before).await;
    let queued = resume_stopped(ctx, &stopped).await;
    show(ctx, &moved?, queued.as_ref())
}

async fn plan_moves(
    ctx: &Context,
    root: Option<PathBuf>,
    regular_dir: bool,
) -> Result<RepositoryMoves> {
    ctx.progress(
        "Checking repositories",
        request::<RepositoryMoves>(
            &ctx.paths,
            RepositoryMethod::PlanRepositoryMoves { root, regular_dir },
        ),
    )
    .await
}

/// Stop the workspaces `executions` run in, keeping their agents' sessions,
/// and add to `stopped` the executions each ran as it stopped, including
/// any started since the plan. An execution Shoal cannot prove ended stops
/// once the user confirms it did, or when `confirmed`.
async fn stop_running(
    ctx: &Context,
    executions: &[Execution],
    confirmed: bool,
    stopped: &mut Vec<Execution>,
) -> Result<()> {
    let workspaces: BTreeSet<_> = executions.iter().map(|e| &e.workspace_id).collect();
    for workspace in workspaces {
        let running = client::inspect(&ctx.paths, workspace.clone())
            .await?
            .executions;
        let result = ctx
            .progress(
                "Stopping commands and agents",
                super::stop::stop(ctx, workspace, confirmed),
            )
            .await;
        // A failed stop may have ended some of them.
        stopped.extend(running);
        result?;
    }
    Ok(())
}

async fn move_repositories(ctx: &Context, regular_dir: bool) -> Result<Vec<MovedRepository>> {
    ctx.progress(
        "Moving repositories",
        request::<Vec<MovedRepository>>(
            &ctx.paths,
            RepositoryMethod::MoveRepositories { regular_dir },
        ),
    )
    .await
}

/// The repositories and workspaces as recorded before the move.
struct Registrations {
    repositories: Vec<Repository>,
    workspaces: Vec<Workspace>,
}

impl Registrations {
    async fn load(ctx: &Context) -> Result<Self> {
        Ok(Self {
            repositories: client::repositories(&ctx.paths).await?,
            workspaces: client::workspaces(&ctx.paths).await?,
        })
    }
}

/// For each repository whose directory moved since `before`, remove agent
/// trust for its old paths and trust its workspaces for the agents whose
/// sessions `shoal resume` continues there. Failures only warn.
async fn retrust(ctx: &Context, before: &Registrations) {
    let after = match Registrations::load(ctx).await {
        Ok(after) => after,
        Err(error) => {
            eprintln!("warning: cannot trust the moved workspaces for their agents: {error:#}");
            return;
        }
    };
    for old in &before.repositories {
        let moved = after
            .repositories
            .iter()
            .any(|repo| repo.id == old.id && repo.workspaces_dir != old.workspaces_dir);
        if !moved {
            continue;
        }
        let paths: Vec<_> = before
            .workspaces
            .iter()
            .filter(|workspace| workspace.repository_id == old.id)
            .map(|workspace| workspace.path.clone())
            .collect();
        agents::cleanup_moved_trust(ctx, old, &paths).await;
        for workspace in after
            .workspaces
            .iter()
            .filter(|workspace| workspace.repository_id == old.id)
        {
            agents::trust_saved(ctx, workspace).await;
        }
    }
}

/// Queue the restores of the agents `executions` ran, warning when that
/// fails, since `shoal resume` still restores them.
async fn resume_stopped(ctx: &Context, executions: &[Execution]) -> Option<Queued> {
    if executions.is_empty() {
        return None;
    }
    match super::resume::queue_stopped(ctx, executions).await {
        Ok(queued) => Some(queued),
        Err(error) => {
            eprintln!("warning: cannot resume the stopped agents; run shoal resume: {error:#}");
            None
        }
    }
}

fn show(ctx: &Context, moved: &Vec<MovedRepository>, queued: Option<&Queued>) -> Result<()> {
    ctx.show(moved, |moved| {
        let palette = Palette::stdout(ctx.json);
        if moved.is_empty() {
            outln!("No repository directories to move");
        }
        for moved in moved {
            let directory = moved.repository.workspaces_dir.as_deref();
            outln!(
                "{}",
                palette.paint(
                    Style::Success,
                    format_args!(
                        "Moved {} to {}",
                        repository::name(&moved.repository),
                        directory.unwrap_or(&moved.repository.path).display()
                    )
                )
            );
        }
        if let Some(summary) = queued.and_then(Queued::summary) {
            outln!("{summary}");
        }
    })
}

fn plan(moves: &RepositoryMoves, workspaces: &[Workspace]) -> String {
    let mut text = format!(
        "Move repository directories into {}:",
        moves.directory.display()
    );
    for repo in &moves.repositories {
        if let Some(directory) = &repo.workspaces_dir {
            text.push_str(&format!(
                "\n  {}: {}",
                repository::name(repo),
                directory.display()
            ));
        }
    }
    if !moves.executions.is_empty() {
        text.push_str("\nStop, then resume after the move:");
    }
    for execution in &moves.executions {
        let workspace = workspaces
            .iter()
            .find(|workspace| workspace.id == execution.workspace_id)
            .map_or(execution.workspace_id.as_str(), |workspace| &workspace.name);
        let what = execution.agent.as_deref().unwrap_or("command");
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
            key: "root_dir".into(),
            value: Some(value.to_string()),
        }],
    )?;
    client::reload_config(&ctx.paths).await?;
    Ok(())
}
