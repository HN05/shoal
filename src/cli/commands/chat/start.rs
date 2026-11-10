//! Starting a chat where the command line asks: in a repository's
//! checkout, in a workspace's worktree, or without a repository in
//! `root_dir`, below the parent it names and on the issue it names.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use super::{Options, Topic, handoff, issue, launch::begin};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{agents::ChatRepository, context::Context, ui},
    config::{Effective, templates},
    model::{Repository, SpaceKind, Workspace},
    protocol::ConfigTarget,
    repository,
};

/// What a chat starts as: its mode, and the issue it starts on, if any,
/// where `Some(None)` asks for one.
pub(in crate::cli::commands) struct Subject {
    pub mode: ui::ModeChoice,
    pub issue: Option<Option<String>>,
    /// The issue an imported chat is linked to, as [`Topic::linked_issue`].
    pub linked_issue: Option<templates::IssueFields>,
}

/// Start a chat about the repository `repository` names, else the issue's,
/// else the current one, else one picked.
pub(in crate::cli::commands) async fn start(
    ctx: &Context,
    repository: Option<String>,
    subject: Subject,
    options: Options,
) -> Result<i32> {
    // A restore runs where nobody may watch it, so any failure reaches the
    // user through the daemon.
    let restores = options.restore.clone();
    let started = async {
        let mut options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let repo = match &subject.issue {
            Some(input) => issue::repository(ctx, input.as_deref(), repository).await?,
            None => chosen(ctx, repository).await?,
        };
        check_parent(ctx, Some(&repo), None, options.parent.as_ref()).await?;
        let settings = settings(ctx, &repo).await?;
        options.check_agent(&settings)?;
        let mode = (subject.mode).chosen(
            ctx,
            &settings.modes,
            ui::ModeFit::in_repository(SpaceKind::Chat),
        )?;
        options.check_given(ctx, Some(repo.id.as_str())).await?;
        let topic = Topic {
            linked_issue: subject.linked_issue,
            ..Topic::repository(&repo, &mode)
        };
        let topic = on_issue(ctx, &repo, &settings, topic, subject.issue).await?;
        begin(
            ctx,
            ChatRepository::Registered(repo),
            settings,
            topic,
            options,
        )
        .await
    }
    .await;
    handoff::report(&started);
    crate::client::chats::keep_failed_restore(&ctx.paths, restores.as_ref(), started).await
}

/// Start a chat about `topic` without a repository, in `root_dir`, which
/// manages the spaces of every repository.
pub(in crate::cli::commands) async fn start_outside_repositories(
    ctx: &Context,
    topic: Topic,
    options: Options,
) -> Result<i32> {
    let restores = options.restore.clone();
    let started = async {
        let options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        check_parent(ctx, None, None, options.parent.as_ref()).await?;
        let repository = ChatRepository::Outside(root_dir(ctx).await?);
        let settings = crate::client::workspaces::settings(&ctx.paths, repository.config()).await?;
        crate::config::modes::resolve(&settings.modes, &topic.mode, SpaceKind::Chat, false)?;
        options.check_given(ctx, None).await?;
        begin(ctx, repository, settings, topic, options).await
    }
    .await;
    handoff::report(&started);
    crate::client::chats::keep_failed_restore(&ctx.paths, restores.as_ref(), started).await
}

/// `root_dir`, where a chat without a repository runs, which the daemon
/// prepares as registering a repository does.
pub(in crate::cli::commands) async fn root_dir(ctx: &Context) -> Result<PathBuf> {
    crate::client::chats::root(&ctx.paths).await
}

/// Start a chat in the worktree of the workspace `workspace` names, else
/// the current one, else one picked among the ready workspaces.
pub(in crate::cli::commands) async fn start_in_workspace(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    subject: Subject,
    options: Options,
) -> Result<i32> {
    let restores = options.restore.clone();
    let started = async {
        let mut options = options.for_caller(ctx)?;
        options.check_name(ctx).await?;
        let workspace = chosen_workspace(ctx, workspace).await?;
        let repo = workspace_repository(ctx, &workspace).await?;
        let parent = options.parent.as_ref();
        check_parent(ctx, Some(&repo), Some(&workspace), parent).await?;
        let settings = settings(ctx, &repo).await?;
        options.check_agent(&settings)?;
        let mode = (subject.mode).chosen(
            ctx,
            &settings.modes,
            ui::ModeFit::in_repository(SpaceKind::Chat),
        )?;
        options.check_given(ctx, Some(repo.id.as_str())).await?;
        let topic = Topic {
            linked_issue: subject.linked_issue,
            ..Topic::workspace(workspace, &mode)
        };
        let topic = on_issue(ctx, &repo, &settings, topic, subject.issue).await?;
        begin(
            ctx,
            ChatRepository::Registered(repo),
            settings,
            topic,
            options,
        )
        .await
    }
    .await;
    handoff::report(&started);
    crate::client::chats::keep_failed_restore(&ctx.paths, restores.as_ref(), started).await
}

/// Refuse `parent`, the space a chat in `repo`, or without one, in the
/// worktree of `workspace` when given, would go below, as its registration
/// would, before a picker or prompt asks for the rest.
async fn check_parent(
    ctx: &Context,
    repo: Option<&Repository>,
    workspace: Option<&Workspace>,
    parent: Option<&SpaceSelector>,
) -> Result<()> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let workspace = workspace.map(|workspace| SpaceSelector::id(&workspace.id));
    let repository = repo.map(|repo| repo.id.clone().into());
    crate::client::chats::check_parent(&ctx.paths, repository, workspace, parent.clone()).await
}

/// `topic` in `repo` started on the issue `issue` names, if any.
async fn on_issue(
    ctx: &Context,
    repo: &Repository,
    settings: &Effective,
    topic: Topic,
    issue: Option<Option<String>>,
) -> Result<Topic> {
    let Some(input) = issue else {
        return Ok(topic);
    };
    let issue = issue::chosen(ctx, repo, &settings.issue, &topic.mode, input).await?;
    Ok(issue::on(topic, repo, &issue))
}

/// The workspace `selector` names, else the current one, else one picked
/// among the ready workspaces.
async fn chosen_workspace(ctx: &Context, selector: Option<SpaceSelector>) -> Result<Workspace> {
    let id = ui::select_ready_workspace(ctx, selector).await?;
    hosting_workspace(ctx, &id).await
}

/// The workspace `selector` names, by number, ID or name, refused before
/// anything is asked unless a chat may run in it; the daemon checks again.
pub(in crate::cli::commands) async fn hosting_workspace(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<Workspace> {
    let workspace = crate::client::workspaces::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| selector.selects(&workspace.id, &workspace.name, workspace.number))
        .with_context(|| format!("no space {selector}"))?;
    ensure_hosts_chat(&workspace)?;
    Ok(workspace)
}

/// Refuse `workspace` unless it is ready with its worktree there.
fn ensure_hosts_chat(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == crate::state::WorkspaceState::Ready,
        "workspace {} is {}, and a chat runs only in a ready workspace",
        workspace.name,
        workspace.state
    );
    ensure!(
        workspace.path.is_dir(),
        "the worktree of workspace {} is missing at {}",
        workspace.name,
        workspace.path.display()
    );
    Ok(())
}

/// The registered repository of `workspace`.
pub(in crate::cli::commands) async fn workspace_repository(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<Repository> {
    crate::client::repositories::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .with_context(|| format!("the repository of workspace {} is missing", workspace.name))
}

/// The repository's settings, which choose the chat's agent and template.
pub(in crate::cli::commands) async fn settings(
    ctx: &Context,
    repo: &Repository,
) -> Result<Effective> {
    crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Repository(repo.id.clone().into()),
    )
    .await
}

/// The named repository, else the current checkout's or workspace's, else a picker.
pub(in crate::cli::commands) async fn chosen(
    ctx: &Context,
    repository: Option<String>,
) -> Result<Repository> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let selector = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => crate::cli::commands::issues::current_repository(
            ctx,
            repos.clone(),
            crate::cli::commands::repositories::REPOSITORY,
        )
        .await?
        .into(),
    };
    Ok(repository::select(&repos, selector).await?.clone())
}

#[cfg(test)]
mod tests;
