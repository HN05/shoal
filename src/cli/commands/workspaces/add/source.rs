//! What an interactive addition starts from: a new or existing branch, an issue or a PR.
use anyhow::{Result, ensure};

use super::{plan::Creation, target::AddTarget};
use crate::{
    cli::{
        client::{self, request},
        commands::{issues, links},
        context::Context,
        ui,
    },
    git::{self, existing_branch::Branch},
    protocol::RepositoryMethod,
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
            let repo = target.repository(ctx).await?;
            let issues = issues::origin_forge(ctx, repo)
                .await?
                .open_issues(&ctx.forge_account()?)
                .await?;
            return ui::pick_item(ctx, "Issue> ", "issues", issues).map(Some);
        }
        Source::Pull => {
            let repo = target.repository(ctx).await?;
            let forge = issues::origin_forge(ctx, repo).await?;
            let account = ctx.forge_account()?;
            let pulls = forge.open_pulls(&account).await?;
            let number = ui::pick_item(ctx, "Pull request> ", "pull requests", pulls)?;
            let pull = forge.pull_request(&account, &number).await?;
            links::pull_creation(ctx, repo, pull, creation).await?;
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
        Some(input) => Ok(Some(
            issues::load(ctx, target.repository(ctx).await?, &input).await?,
        )),
        None => Ok(None),
    }
}

pub(super) async fn select_add_creation(
    ctx: &Context,
    target: &AddTarget,
    mut creation: Creation,
    issue: Option<&issues::Issue>,
) -> Result<Creation> {
    if creation.branch.is_some() || creation.existing.is_some() {
        return Ok(creation);
    }
    if let Some(issue) = issue {
        return issue_creation(ctx, target, creation, issue.branch_name(), &issue.url).await;
    }
    // Check here so a typo can be corrected before creating work.
    creation.branch = Some(loop {
        let name = ui::input(ctx, "Branch name")?;
        match git::check_branch_name(None, &name).await {
            Ok(()) => break name,
            Err(error) => eprintln!("{error:#}"),
        }
    });
    Ok(creation)
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
    let workspaces = client::workspaces(&ctx.paths).await?;
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
            associated = Some(workspace.branch.clone());
        }
    }
    if let Some(branch) = associated {
        creation.existing = Some(branch);
        return Ok(creation);
    }
    if let Some(workspace) = workspaces
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == name)
    {
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
    let branches = request::<Vec<Branch>>(
        &ctx.paths,
        RepositoryMethod::ListBranches {
            repository: target.selector.clone(),
        },
    )
    .await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
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
                .find(|w| w.repository_id == repo.id && w.branch == b.name)
            {
                Some(w) => format!("{label}  [workspace: {}]", w.name),
                None => label,
            };
            (b.selector(), label)
        })
        .collect();
    ui::pick(ctx, "Branch> ", entries)
}
