//! Pasted links and item selectors share repository and workspace workflows.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::workspaces::{self, AgentLaunch, Creation};
use crate::{
    agent::Agent,
    cli::{
        args::items::{ItemArgs, RelationArgs},
        client,
        context::Context,
        ui,
    },
    forge::{
        IssueInput, PullRequest,
        items::link::{ItemKind, Link, LinkTarget, Selection},
    },
    git,
    model::{Relation, Repository, ReviewMark, Selected, SelectedItem, Workspace},
    protocol::{AssignmentMethod, ItemContext, ItemMethod},
};

/// The workspace and the items selected in it, with the kind of a bare number
/// resolved.
pub(super) async fn select(ctx: &Context, items: ItemArgs) -> Result<(String, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    let selection = resolve_kind(ctx, &workspace, selection).await?;
    Ok((workspace, selection))
}

/// The items selected in the named or current workspace, with the kind of a
/// bare number resolved, or in the registered checkout the current directory
/// is in, where only a number or URL selects one.
pub(super) async fn select_in_context(
    ctx: &Context,
    items: ItemArgs,
) -> Result<(ItemContext, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    match super::items::select_context(ctx, items.workspace).await? {
        ItemContext::Workspace(workspace) => {
            let selection = resolve_kind(ctx, &workspace, selection).await?;
            Ok((ItemContext::Workspace(workspace), selection))
        }
        checkout => Ok((checkout, selection)),
    }
}

/// The items `selection` names: in the workspace, where it may name linked
/// ones, or in the repository of the registered checkout.
pub(super) async fn select_items(
    ctx: &Context,
    context: &ItemContext,
    selection: Selection,
) -> Result<Selected> {
    match context {
        ItemContext::Workspace(workspace) => {
            let workspace = workspace.clone();
            let method = ItemMethod::SelectItems {
                workspace,
                selection,
            };
            client::request(&ctx.paths, method).await
        }
        ItemContext::Checkout(repository) => checkout_selected(ctx, repository, selection).await,
    }
}

/// The item `selection` names in the repository of its registered checkout,
/// where nothing is linked.
async fn checkout_selected(
    ctx: &Context,
    repository: &str,
    selection: Selection,
) -> Result<Selected> {
    let input = selection
        .input
        .context("outside a workspace, nothing is linked; pass a number or URL")?;
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == repository)
        .context("the checkout's repository is no longer registered")?;
    let forge = super::issues::origin_forge(ctx, &repo).await?;
    let kind = match selection.kind {
        Some(kind) => kind,
        None => {
            forge
                .item_kind(&ctx.forge_account()?, input.parse()?)
                .await?
        }
    };
    let (_, url) = match kind {
        ItemKind::Issue => forge.issue(&input)?,
        ItemKind::Pr => forge.pull(&input)?,
    };
    Ok(Selected {
        path: repo.path,
        items: vec![SelectedItem { url, kind }],
    })
}

/// Give a number selected without a kind the kind of the linked item with
/// that number, otherwise the kind the forge reports for it.
async fn resolve_kind(ctx: &Context, workspace: &str, selection: Selection) -> Result<Selection> {
    let (None, Some(input)) = (selection.kind, &selection.input) else {
        return Ok(selection);
    };
    let number = input.parse::<u64>()?;
    let kind = match linked_kind(ctx, workspace, number).await? {
        Some(kind) => kind,
        None => {
            let repo = workspace_repository(ctx, workspace).await?;
            super::issues::origin_forge(ctx, &repo)
                .await?
                .item_kind(&ctx.forge_account()?, number)
                .await?
        }
    };
    Ok(Selection {
        kind: Some(kind),
        ..selection
    })
}

/// The kind of the workspace's item numbered `number` under any relation.
async fn linked_kind(ctx: &Context, workspace: &str, number: u64) -> Result<Option<ItemKind>> {
    let mut linked: Selected = client::request(
        &ctx.paths,
        ItemMethod::SelectItems {
            workspace: workspace.to_owned(),
            selection: Selection::default(),
        },
    )
    .await?;
    let links = workspace_record(ctx, workspace).await?.links;
    let recorded = links.opened.into_iter().chain(links.related);
    linked.items.extend(recorded.map(|item| SelectedItem {
        url: item.url,
        kind: item.kind,
    }));
    // A number names an item of the workspace's repository; issues opened
    // in another one share its numbers.
    let remote = crate::repository::remote_url_from_path(&linked.path)
        .await?
        .context("issue lookup needs an origin remote")?;
    linked.kind_of(&remote, number)
}

pub(super) async fn link(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let selected = Selection::parse(items.kind_or_url, items.item)?;
    ensure!(
        selected.input.is_some() || ctx.interactive(),
        "link needs an issue or PR number or URL"
    );
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    let selected = resolve_kind(ctx, &workspace, selected).await?;
    let (kind, input) = match selected {
        Selection {
            kind: Some(kind),
            input: Some(input),
        } => (kind, input),
        Selection { kind, .. } => pick_open_item(ctx, &workspace, kind).await?,
    };
    let title = match kind {
        ItemKind::Issue => issue_title(ctx, &workspace, &input).await,
        ItemKind::Pr => None,
    };
    client::request::<()>(
        &ctx.paths,
        ItemMethod::LinkItem {
            workspace,
            kind,
            url: input,
            relation,
            title,
        },
    )
    .await?;
    ctx.emit(
        link_outcome(kind, relation),
        serde_json::json!({"registered": true, "relation": relation}),
    )?;
    Ok(0)
}

fn link_outcome(kind: ItemKind, relation: Relation) -> &'static str {
    match (relation, kind) {
        (Relation::Assigned, ItemKind::Pr) => {
            "PR linked as assigned; run shoal watch pr for updates and shoal done once the assignment is finished."
        }
        (Relation::Assigned, ItemKind::Issue) => {
            "Issue linked as assigned; closure can complete the assignment."
        }
        (Relation::Opened, ItemKind::Pr) => "PR linked as opened.",
        (Relation::Opened, ItemKind::Issue) => "Issue linked as opened.",
        (Relation::Related, ItemKind::Pr) => "PR linked as related.",
        (Relation::Related, ItemKind::Issue) => "Issue linked as related.",
    }
}

/// The title lists show for the workspace; linking proceeds without one when
/// the forge cannot be read.
async fn issue_title(ctx: &Context, workspace: &str, input: &str) -> Option<String> {
    let title = async {
        let repo = workspace_repository(ctx, workspace).await?;
        anyhow::Ok(super::issues::load(ctx, &repo, input).await?.title)
    };
    match title.await {
        Ok(title) => Some(title),
        Err(error) => {
            eprintln!("Issue title unavailable: {error:#}");
            None
        }
    }
}

async fn workspace_record(ctx: &Context, workspace: &str) -> Result<Workspace> {
    client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|w| w.id == workspace || w.name == workspace)
        .with_context(|| format!("unknown workspace: {workspace}"))
}

async fn workspace_repository(ctx: &Context, workspace: &str) -> Result<Repository> {
    let repository = workspace_record(ctx, workspace).await?.repository_id;
    client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == repository)
        .context("workspace repository is missing")
}

/// Choose an open issue or PR of the workspace's repository, asking for the
/// kind when it is not given.
async fn pick_open_item(
    ctx: &Context,
    workspace: &str,
    kind: Option<ItemKind>,
) -> Result<(ItemKind, String)> {
    let kind = match kind {
        Some(kind) => kind,
        None => ui::pick_choice(
            ctx,
            "Link> ",
            &[(ItemKind::Issue, "Issue"), (ItemKind::Pr, "Pull request")],
        )?,
    };
    let repo = workspace_repository(ctx, workspace).await?;
    let forge = super::issues::origin_forge(ctx, &repo).await?;
    let number = match kind {
        ItemKind::Issue => {
            let issues = forge.open_issues(&ctx.forge_account()?).await?;
            ui::pick_item(ctx, "Issue> ", "issues", issues)?
        }
        ItemKind::Pr => {
            let pulls = forge.open_pulls(&ctx.forge_account()?).await?;
            ui::pick_item(ctx, "Pull request> ", "pull requests", pulls)?
        }
    };
    Ok((kind, number))
}

pub(super) async fn unlink(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let (workspace, selected) = select(ctx, items).await?;
    client::request::<()>(
        &ctx.paths,
        ItemMethod::UnlinkItems {
            workspace,
            relation,
            kind: selected.kind,
            url: selected.input,
        },
    )
    .await?;
    ctx.emit("Items unlinked", serde_json::json!({"registered": false}))?;
    Ok(0)
}

pub(super) async fn watch(
    ctx: &Context,
    items: ItemArgs,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    let (workspace, selected) = select(ctx, items).await?;
    workspaces::watch_items(ctx, workspace, selected, timeout, resumed).await
}

pub(super) async fn ready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let (workspace, selection) = select(ctx, items).await?;
    let marks: Vec<ReviewMark> = client::request(
        &ctx.paths,
        AssignmentMethod::MarkReady {
            workspace,
            selection,
        },
    )
    .await?;
    for warning in marks.iter().flat_map(|mark| &mark.warnings) {
        eprintln!("{warning}");
    }
    ctx.emit(
        &format!("Ready for review:\n{}", review_lines(&marks)),
        &marks,
    )?;
    Ok(0)
}

/// `draft` overrides `[unready] draft` when given.
pub(super) async fn unready(ctx: &Context, items: ItemArgs, draft: Option<bool>) -> Result<i32> {
    let (workspace, selection) = select(ctx, items).await?;
    let marks: Vec<ReviewMark> = client::request(
        &ctx.paths,
        AssignmentMethod::ClearReady {
            workspace,
            selection,
            draft,
        },
    )
    .await?;
    let message = format!("Ready-for-review marks cleared:\n{}", review_lines(&marks));
    ctx.emit(&message, &marks)?;
    Ok(0)
}

/// One indented line per mark: its item, or the workspace, and whether new
/// commits arrived since.
pub(super) fn review_lines(marks: &[ReviewMark]) -> String {
    marks
        .iter()
        .map(|mark| {
            let item = match (mark.kind, &mark.url) {
                (Some(kind), Some(url)) => format!("{kind} {url}"),
                _ => "workspace".to_owned(),
            };
            let outdated = if mark.stale == Some(true) {
                " (outdated)"
            } else {
                ""
            };
            format!("  {item}{outdated}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `positional` is a pasted issue, PR or branch URL of any forge,
/// rather than a repository.
fn is_item_link(positional: &str) -> bool {
    const ROUTES: &[&str] = &[
        "/issues/",
        "/pull/",
        "/pulls/",
        "/-/merge_requests/",
        "/pull-requests/",
        "/tree/",
        "/branch/",
    ];
    IssueInput::parse(positional) == IssueInput::Url
        && ROUTES.iter().any(|route| positional.contains(route))
}

pub(super) struct AddInput {
    pub positional: Option<String>,
    pub repository: Option<String>,
    pub issue: Option<String>,
}

pub(super) async fn add(
    ctx: &Context,
    input: AddInput,
    mut creation: Creation,
    agent: Option<Agent>,
    args: Vec<OsString>,
    options: workspaces::AddOptions,
) -> Result<i32> {
    let Some(positional) = input.positional.as_deref() else {
        return workspaces::add(
            ctx,
            input.repository,
            creation,
            input.issue,
            AgentLaunch::Add(agent),
            args,
            options,
        )
        .await;
    };
    let number = IssueInput::parse(positional) == IssueInput::Number
        && creation.branch.is_none()
        && creation.existing.is_none()
        && input.issue.is_none();
    let pasted = is_item_link(positional);
    if !number && !pasted {
        ensure!(
            input.repository.is_none(),
            "a positional repository cannot be combined with --repo"
        );
        return workspaces::add(
            ctx,
            input.positional,
            creation,
            input.issue,
            AgentLaunch::Add(agent),
            args,
            options,
        )
        .await;
    }
    ensure!(
        creation.branch.is_none() && creation.existing.is_none() && input.issue.is_none(),
        "an item or branch link cannot be combined with a branch, --existing or --issue"
    );
    let link = pasted.then(|| Link::parse(positional)).transpose()?;
    let target = link.as_ref().map(|link| &link.target);
    let issue = matches!(target, None | Some(LinkTarget::Issue));
    if issue && input.repository.is_some() {
        // Resolve like --issue, which offers to register an unknown repository.
        return workspaces::add(
            ctx,
            input.repository,
            creation,
            Some(positional.into()),
            AgentLaunch::IssueDefault(agent),
            args,
            options,
        )
        .await;
    }
    let mut repos = client::repositories(&ctx.paths).await?;
    let repository = match input.repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => match &link {
            Some(link) => super::issues::registered_remote(
                ctx,
                &mut repos,
                link.repository.clone(),
                "shoal add <link> --repo <repository>",
            )
            .await?
            .into(),
            None => {
                super::issues::current_repository(ctx, repos.clone(), super::issues::REPO_OR_URL)
                    .await?
                    .into()
            }
        },
    };
    let repo = crate::repository::select(&repos, &repository).await?;
    if issue {
        return workspaces::add(
            ctx,
            Some(repo.id.clone()),
            creation,
            Some(positional.into()),
            AgentLaunch::IssueDefault(agent),
            args,
            options,
        )
        .await;
    }
    let forge = super::issues::origin_forge(ctx, repo).await?;
    ensure!(
        link.as_ref().is_some_and(|link| link.repository == forge),
        "link belongs to a different repository"
    );
    match target.unwrap() {
        LinkTarget::Pr => {
            let pull = forge
                .pull_request(&ctx.forge_account()?, positional)
                .await?;
            pull_creation(ctx, repo, pull, &mut creation).await?;
        }
        LinkTarget::Branch(branch) => {
            git::check_branch_name(None, branch).await?;
            creation.existing = Some(git::remote_ref("origin", branch));
        }
        LinkTarget::Issue => unreachable!(),
    }
    workspaces::add(
        ctx,
        Some(repo.id.clone()),
        creation,
        None,
        AgentLaunch::Add(agent),
        args,
        options,
    )
    .await
}

/// Reopen the workspace that owns the PR's head branch, or open the pushed
/// branch compared against the PR's base; either way the PR is linked.
pub(super) async fn pull_creation(
    ctx: &Context,
    repo: &Repository,
    pull: PullRequest,
    creation: &mut Creation,
) -> Result<()> {
    let owned = client::workspaces(&ctx.paths)
        .await?
        .iter()
        .any(|workspace| workspace.repository_id == repo.id && workspace.branch == pull.head);
    if owned {
        creation.existing = Some(pull.head);
    } else {
        if creation.base.is_none() {
            creation.base =
                Some(super::review::fetch_pull_branch(&repo.path, "base", &pull.base).await?);
        }
        creation.existing = Some(git::remote_ref("origin", &pull.head));
    }
    creation.pr = Some(pull.url);
    Ok(())
}

#[cfg(test)]
mod tests;
