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
    let repositories = client::repositories(&ctx.paths).await?;
    let moves = plan_moves(ctx, root.clone(), regular_dir).await?;
    if !confirmed && !moves.repositories.is_empty() {
        let workspaces = client::workspaces(&ctx.paths).await?;
        ensure!(
            ui::confirm(ctx, &plan(&moves, &workspaces), "--yes")?,
            "repository move canceled"
        );
    }
    if let Some(root) = root {
        save_root(ctx, &root).await?;
    }
    let moved = async {
        stop_running(ctx, &moves.executions, confirmed).await?;
        move_repositories(ctx, regular_dir).await
    }
    .await;
    if let Ok(moved) = &moved {
        retrust(ctx, &repositories, moved).await;
    }
    let queued = resume_stopped(ctx, &moves.executions).await;
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

/// Stop the workspaces `executions` run in, keeping their agents' sessions.
/// An execution Shoal cannot prove ended stops once the user confirms it
/// did, or when `confirmed`.
async fn stop_running(ctx: &Context, executions: &[Execution], confirmed: bool) -> Result<()> {
    let workspaces: BTreeSet<_> = executions.iter().map(|e| &e.workspace_id).collect();
    for workspace in workspaces {
        ctx.progress(
            "Stopping commands and agents",
            super::stop::stop(ctx, workspace, confirmed),
        )
        .await?;
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

/// Remove agent trust for the old paths of the `moved` repositories, as
/// `repositories` recorded them, and trust each moved workspace for the
/// agents whose sessions `shoal resume` continues there.
async fn retrust(ctx: &Context, repositories: &[Repository], moved: &[MovedRepository]) {
    for moved in moved {
        if let Some(old) = repositories
            .iter()
            .find(|repo| repo.id == moved.repository.id)
        {
            agents::cleanup_repository_trust(ctx, old, &moved.workspace_paths).await;
        }
    }
    let workspaces = match client::workspaces(&ctx.paths).await {
        Ok(workspaces) => workspaces,
        Err(error) => {
            eprintln!("warning: cannot trust the moved workspaces for their agents: {error:#}");
            return;
        }
    };
    for workspace in workspaces.iter().filter(|workspace| {
        moved
            .iter()
            .any(|moved| moved.repository.id == workspace.repository_id)
    }) {
        agents::trust_saved(ctx, workspace).await;
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
