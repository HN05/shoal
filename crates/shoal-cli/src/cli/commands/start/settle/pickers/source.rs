//! What a new workspace starts from when nothing names it, picked on a
//! terminal: a new or existing branch, an issue or a PR.
use crate::git::existing_branch::BranchExt;
use anyhow::Result;
use shoal_protocol::launches::{ItemChoice, Launch, StartTarget};

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    config::modes::ModeName,
    forge::items::{link::ItemKind, list::Search, reference::ItemInput},
    launches::workspace,
    model::Repository,
    start_plan::Creation,
};

/// Pick on a terminal what the new workspace of `launch` in `repository`
/// starts from when it names nothing: an existing branch or PR, or an
/// issue. A new branch it leaves unnamed, which the daemon begins as a
/// chat whose agent makes it a workspace once it knows the work, so what
/// only a branch takes is refused before another picker asks. Returns
/// whether it picked one.
pub(in crate::cli::commands::start::settle) async fn pick_source(
    ctx: &Context,
    launch: &mut Launch,
    repository: Option<&Repository>,
) -> Result<bool> {
    let repo = repository.filter(|_| names_nothing(launch) && ctx.interactive());
    if let Some(repo) = repo {
        let mut creation = Creation::default();
        let picked = pick_add_source(ctx, repo, &mut creation).await?;
        picked_source(launch, creation, picked);
    }
    ensure_branch_takes(launch)?;
    Ok(repo.is_some())
}

/// Refuse what only a new workspace's branch takes in `launch` when it
/// names none, so it begins as a chat.
fn ensure_branch_takes(launch: &Launch) -> Result<()> {
    match names_no_branch(launch) {
        true => crate::launches::ensure_begins_as_chat(launch),
        false => Ok(()),
    }
}

/// Settle on a terminal what the new workspace a creation `launch` makes
/// starts in and from, as a start's pickers do: its repository, picked
/// when nothing names one, checked with the daemon, and, when it names
/// nothing its branch comes from, its source, a new branch's name
/// included; without a terminal, refuse a launch that names no source.
pub(in crate::cli::commands) async fn pick_start_source(
    ctx: &Context,
    launch: &mut Launch,
) -> Result<()> {
    launch.target = crate::launches::decided_target(launch)?;
    if !ctx.interactive() {
        anyhow::ensure!(
            !names_no_branch(launch),
            "name the workspace's branch with --branch, --issue or an issue, PR or branch link"
        );
        return Ok(());
    }
    let repo = super::super::repository::chosen(ctx, launch).await?;
    // The daemon checks a creation as the workspace of a worker's start.
    let checked = Launch {
        mode: Some(ModeName::DEFAULT),
        ..launch.clone()
    };
    crate::client::launches::check_creation(&ctx.paths, checked).await?;
    if !names_no_branch(launch) {
        return Ok(());
    }
    let mut creation = Creation::default();
    let picked = pick_add_source(ctx, &repo, &mut creation).await?;
    picked_source(launch, creation, picked);
    // A new branch the picker chose still needs its name.
    if names_no_branch(launch) {
        let check = async |name: String| shoal_core::git::check_branch_name(&name).map(|()| name);
        launch.branch = Some(ui::input_until(ctx, "Branch name", None, check).await?);
    }
    Ok(())
}

/// Whether `launch` names nothing a new workspace starts from: no branch,
/// issue, link, base or copy; a repository alone names none.
fn names_nothing(launch: &Launch) -> bool {
    names_no_branch(launch) && launch.base.is_none()
}

/// Whether `launch` names nothing its workspace's branch comes from: no
/// existing workspace, which has its own, branch, issue, PR, link or copy; a
/// base or a repository alone settles no branch.
fn names_no_branch(launch: &Launch) -> bool {
    let names_link = !matches!(launch.target, None | Some(StartTarget::Repository(_)));
    launch.in_workspace.is_none()
        && launch.branch.is_none()
        && launch.issue.is_none()
        && launch.pr.is_none()
        && launch.copy.is_none()
        && launch.checkpoint.is_none()
        && !names_link
}

/// `launch` starting from what a picker chose: the existing branch or PR
/// `creation` opens, or the issue `picked`.
fn picked_source(launch: &mut Launch, creation: Creation, picked: Option<ItemInput>) {
    if let Some(issue) = picked {
        launch.issue = Some(ItemChoice::Named(issue));
    }
    match creation.pr {
        Some(ItemInput::Url(pull)) => {
            launch.target = Some(StartTarget::Pull(pull));
        }
        _ => {
            let picked = creation.existing.as_deref().map(switched_branch);
            launch.branch = launch.branch.take().or(picked);
        }
    }
}

/// The branch the picked ref `selector` names, as the daemon settles a
/// start's branch: a local branch by its name, and a remote's by its whole
/// ref, which keeps the remote it was picked from.
fn switched_branch(selector: &str) -> String {
    crate::git::strip_local(selector)
        .unwrap_or(selector)
        .to_owned()
}

/// Ask what a new workspace in `repo` starts from. Branch choices fill
/// `creation`; a chosen issue's number is returned.
async fn pick_add_source(
    ctx: &Context,
    repo: &Repository,
    creation: &mut Creation,
) -> Result<Option<ItemInput>> {
    #[derive(Clone, Copy)]
    enum Source {
        New,
        Existing,
        Issue,
        Pull,
    }
    let source = ui::pick_choice(
        ctx,
        "Workspace> ",
        &[
            (Source::New, "Create a new branch"),
            (Source::Existing, "Use an existing branch"),
            (Source::Issue, "Use an issue"),
            (Source::Pull, "Use a pull request"),
        ],
    )?;
    match source {
        Source::New => {}
        Source::Existing => creation.existing = Some(pick_branch(ctx, repo).await?),
        Source::Issue => {
            let context = issues::read_context(ctx, repo).await?;
            let search = Search::picker();
            let issues = client::list_items(&ctx.paths, context.into(), ItemKind::Issue, search);
            let number = ui::pick_item(ctx, "Issue> ", "issues", issues.await?)?;
            return Ok(Some(ItemInput::Number(number)));
        }
        Source::Pull => {
            let context = issues::read_context(ctx, repo).await?;
            let search = Search::picker();
            let pulls =
                client::list_items(&ctx.paths, context.clone().into(), ItemKind::Pr, search);
            let number = ui::pick_item(ctx, "Pull request> ", "pull requests", pulls.await?)?;
            let pull = client::pull_request(&ctx.paths, context, ItemInput::Number(number));
            let pull = pull.await?;
            let workspaces = crate::client::spaces::list(&ctx.paths).await?;
            let workspaces = workspaces.into_workspaces();
            workspace::pull_creation(&workspaces, repo, pull, creation).await?;
        }
    }
    Ok(None)
}

/// An existing branch of `repo`, local or remote, picked with the
/// workspace that has it.
async fn pick_branch(ctx: &Context, repo: &Repository) -> Result<String> {
    let branches = crate::client::repositories::branches(&ctx.paths, (&repo.id).into()).await?;
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let entries = branches
        .into_iter()
        .map(|b| {
            let label = match &b.remote {
                Some(remote) => format!("{remote}/{}  (remote)", b.name),
                None => format!("{}  (local)", b.name),
            };
            let label = match workspaces
                .iter()
                .find(|w| w.repository() == &repo.id && w.branch() == Some(&b.name))
            {
                Some(w) => format!("{label}  [workspace: {}]", w.name),
                None => label,
            };
            (b.selector(), label)
        })
        .collect();
    ui::pick(ctx, "Branch> ", entries)
}

#[cfg(test)]
mod tests;
