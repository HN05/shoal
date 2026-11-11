//! What an interactive addition starts from: a new or existing branch, an issue or a PR.
use crate::git::existing_branch::BranchExt;
use anyhow::{Result, ensure};

use super::target::AddTarget;
use crate::{
    cli::{
        client::{self},
        commands::issues,
        context::Context,
        ui,
    },
    forge::items::{link::ItemKind, list::Search},
    git::{self},
    protocol::ItemContext,
    start_plan::Creation,
    state::WorkspaceState,
};

/// Ask what an interactive addition starts from when no option says. Branch
/// choices fill `creation`; a chosen issue's number is returned.
pub(super) async fn pick_add_source(
    ctx: &Context,
    target: &AddTarget,
    creation: &mut Creation,
) -> Result<Option<String>> {
    if creation.branch.is_some()
        || creation.existing.is_some()
        || creation.base.is_some()
        || creation.pull_head.is_some()
        || creation.copy.is_some()
        || !ctx.interactive()
    {
        return Ok(None);
    }
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
        Source::Existing => creation.existing = Some(pick_add_branch(ctx, target).await?),
        Source::Issue => {
            let context = read_context(ctx, target).await?;
            let search = Search::picker();
            let issues = client::list_items(&ctx.paths, context, ItemKind::Issue, search, None);
            return ui::pick_item(ctx, "Issue> ", "issues", issues.await?).map(Some);
        }
        Source::Pull => {
            let context = read_context(ctx, target).await?;
            let search = Search::picker();
            let pulls = client::list_items(&ctx.paths, context.clone(), ItemKind::Pr, search, None);
            let number = ui::pick_item(ctx, "Pull request> ", "pull requests", pulls.await?)?;
            let pull = client::pull_request(&ctx.paths, context, number).await?;
            let repo = target.repository(ctx).await?;
            super::link::pull_creation(ctx, repo, pull, creation).await?;
        }
    }
    Ok(None)
}

/// Look the addition's issue up before any other picker or handoff, so its
/// failures end there.
pub(super) async fn load_add_issue(
    ctx: &Context,
    target: &AddTarget,
    input: Option<String>,
) -> Result<Option<issues::Issue>> {
    match input {
        Some(input) => {
            let context = read_context(ctx, target).await?;
            Ok(Some(issues::load(ctx, context, &input).await?))
        }
        None => Ok(None),
    }
}

/// Where the addition's forge reads run.
async fn read_context(ctx: &Context, target: &AddTarget) -> Result<ItemContext> {
    issues::read_context(ctx, target.repository(ctx).await?).await
}

/// The branch an issue's addition opens, unless one is chosen already.
pub(super) async fn select_add_creation(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
    issue: Option<&issues::Issue>,
) -> Result<Creation> {
    match issue {
        Some(issue) if creation.unsettled() => {
            issue_creation(ctx, target, creation, issue.branch_name(), &issue.url).await
        }
        _ => Ok(creation),
    }
}

/// Reopen the local branch or workspace that earlier work on the issue left
/// instead of suffixing a new branch, failing here when it cannot be reopened.
async fn issue_creation(
    ctx: &Context,
    target: &AddTarget,
    mut creation: Creation,
    name: String,
    issue_url: &str,
) -> Result<Creation> {
    let repo = target.repository(ctx).await?;
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let mut associated = None;
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.repository_id == repo.id)
    {
        if workspace.links.issue.as_deref() == Some(issue_url) {
            ensure!(
                associated.is_none(),
                "multiple workspaces are associated with this issue; pass an explicit branch"
            );
            ensure!(
                workspace.state == WorkspaceState::Ready,
                "workspace {} is {}; inspect or set it up before reopening",
                workspace.name,
                workspace.state
            );
            associated = Some(
                workspace
                    .require_branch("reopening for an issue")?
                    .to_owned(),
            );
        }
    }
    if let Some(branch) = associated {
        creation.existing = Some(branch);
        return Ok(creation);
    }
    if let Some(workspace) = workspaces.into_iter().find(|workspace| {
        workspace.repository_id == repo.id && workspace.branch.as_ref() == Some(&name)
    }) {
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}; inspect or set it up before reopening",
            workspace.name,
            workspace.state
        );
    } else if git::ref_exists(&repo.path, &git::local_ref(&name), git::isolated_command).await? {
        git::existing_branch::ensure_not_checked_out(&repo.path, &name).await?;
    } else {
        creation.branch = Some(name);
        return Ok(creation);
    }
    creation.existing = Some(name);
    Ok(creation)
}

async fn pick_add_branch(ctx: &Context, target: &AddTarget) -> Result<String> {
    let branches =
        crate::client::repositories::branches(&ctx.paths, target.selector.clone()).await?;
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let repo = target.repository(ctx).await?;
    let entries = branches
        .into_iter()
        .map(|b| {
            let label = match &b.remote {
                Some(remote) => format!("{remote}/{}  (remote)", b.name),
                None => format!("{}  (local)", b.name),
            };
            let label = match workspaces
                .iter()
                .find(|w| w.repository_id == repo.id && w.branch.as_ref() == Some(&b.name))
            {
                Some(w) => format!("{label}  [workspace: {}]", w.name),
                None => label,
            };
            (b.selector(), label)
        })
        .collect();
    ui::pick(ctx, "Branch> ", entries)
}
