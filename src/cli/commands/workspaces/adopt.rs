//! Adopting a worktree, or reclaiming a recorded one whose ownership is unverified.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client::{self, request},
        commands::repositories,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    daemon::recovery::{DirectoryState, ReconcileOptions, Report},
    git,
    model::Workspace,
    protocol::{RecoveryMethod, WorkspaceMethod},
    repository, shell,
};

pub(in crate::cli::commands) async fn adopt(
    ctx: &Context,
    path: PathBuf,
    repository: Option<String>,
    copy: bool,
) -> Result<i32> {
    let path = repositories::absolute(ctx, path)?;
    if !copy && let Some(workspace) = unverified_workspace_at(ctx, &path).await? {
        ensure_recorded_repository(ctx, &workspace, repository.as_deref()).await?;
        return reclaim(ctx, workspace).await;
    }
    let repository = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => worktree_repository(ctx, &path).await?.into(),
    };
    let workspace = request::<Workspace>(
        &ctx.paths,
        WorkspaceMethod::AdoptWorkspace {
            repository,
            path,
            copy,
        },
    )
    .await?;
    ctx.emit(
        &format!(
            "{} {} on branch {} at {} (normal cleanup applies)",
            if copy {
                "Copied and adopted"
            } else {
                "Adopted"
            },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    Ok(0)
}

/// The workspace recorded at `path` whose worktree exists but whose ownership
/// Shoal cannot verify, which adopting it again reclaims.
async fn unverified_workspace_at(ctx: &Context, path: &Path) -> Result<Option<Workspace>> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let Some(workspace) = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.path == path)
    else {
        return Ok(None);
    };
    let reports = reconcile(ctx, &workspace, ReconcileOptions::default()).await?;
    Ok(reports
        .iter()
        .any(|report| report.directory == DirectoryState::Unverified)
        .then_some(workspace))
}

/// Refuse a `--repo` other than the repository `workspace` is recorded in.
async fn ensure_recorded_repository(
    ctx: &Context,
    workspace: &Workspace,
    repository: Option<&str>,
) -> Result<()> {
    let Some(selector) = repository else {
        return Ok(());
    };
    let repositories = client::repositories(&ctx.paths).await?;
    let selected =
        repository::select(&repositories, ui::repository_selector(selector.to_owned())?).await?;
    ensure!(
        selected.id == workspace.repository_id,
        "{} is recorded in another repository than {selector}",
        workspace.name
    );
    Ok(())
}

/// Re-establish ownership of `workspace`'s worktree at its recorded path,
/// which the user confirmed by adopting it, then repair it as doctor did.
async fn reclaim(ctx: &Context, workspace: Workspace) -> Result<i32> {
    let options = ReconcileOptions {
        repair: true,
        reclaim: true,
        ..Default::default()
    };
    let reports = reconcile(ctx, &workspace, options).await?;
    let issues: Vec<_> = reports
        .iter()
        .flat_map(|report| report.issues.iter().map(String::as_str))
        .collect();
    ensure!(
        issues.is_empty(),
        "could not reclaim {}: {}",
        workspace.name,
        issues.join("; ")
    );
    ctx.emit(
        &format!(
            "Reclaimed {} at {}",
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.path.display()
        ),
        client::inspect(&ctx.paths, workspace.id).await?.workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    Ok(0)
}

async fn reconcile(
    ctx: &Context,
    workspace: &Workspace,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    request(
        &ctx.paths,
        RecoveryMethod::Reconcile {
            workspace: Some(workspace.id.clone()),
            options,
        },
    )
    .await
}

/// The registered repository whose checkout owns the worktree at `path`,
/// otherwise the only one sharing its origin remote, otherwise one chosen
/// interactively.
async fn worktree_repository(ctx: &Context, path: &Path) -> Result<String> {
    let mut repos = client::repositories(&ctx.paths).await?;
    let common = git::common_dir(path)
        .await
        .with_context(|| format!("{} is not a Git worktree", path.display()))?;
    for repo in &repos {
        // Checkouts may keep their Git directory elsewhere; an unreadable one owns nothing.
        if git::common_dir(&repo.path)
            .await
            .is_ok_and(|dir| dir == common)
        {
            return Ok(repo.id.clone());
        }
    }
    if let Some(remote) = repository::identity(&path.to_string_lossy()).await? {
        let mut matches = Vec::new();
        for repo in &repos {
            // An unreadable registration cannot share the remote.
            if repository::identity(&repo.source)
                .await
                .ok()
                .flatten()
                .as_ref()
                == Some(&remote)
            {
                matches.push(repo.clone());
            }
        }
        match matches.as_slice() {
            [repo] => return Ok(repo.id.clone()),
            [] => {}
            _ => repos = matches,
        }
    }
    ensure!(
        ctx.interactive(),
        "cannot infer the repository of {}; pass --repo <repository>",
        path.display()
    );
    ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)
}
