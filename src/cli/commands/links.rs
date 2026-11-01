//! Pasted links and item selectors share repository and workspace workflows.
use std::ffi::OsString;

use anyhow::{Context as _, Result, bail, ensure};

use super::workspaces::{self, AgentLaunch, Creation};
use crate::client::output::workspaces::Registered;
use crate::{
    agent::Agent,
    cli::{
        args::items::{ItemArgs, RelationArgs},
        client,
        context::Context,
        ui,
    },
    daemon::refusal::{Reason, Refusal},
    forge::{
        ForgeRepo, IssueInput, PullRequest,
        items::link::{ItemKind, Link, LinkKind, LinkTarget, Selection},
    },
    git,
    model::{PresentSpace, Relation, Repository, ReviewMark, Selected, SelectedItem},
    protocol::{AssignmentMethod, ItemContext, ItemMethod},
};

/// The space and the items selected in it, with the kind of a bare number
/// resolved.
pub(super) async fn select(ctx: &Context, items: ItemArgs) -> Result<(String, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory).await?;
    let selection = resolve_kind(ctx, &space, selection).await?;
    Ok((space, selection))
}

/// The items selected in the named or current space, with the kind of a bare
/// number resolved, or in the checkout of the registered repository `repo`
/// names or the current directory or a URL is in, where only a number or URL
/// selects one.
pub(super) async fn select_in_context(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
) -> Result<(ItemContext, Selection)> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let input = selection.input.as_deref();
    match super::items::select_space_item_context(ctx, items.space, repo, input).await? {
        ItemContext::Workspace(space) | ItemContext::Chat(space) => {
            let resolved = resolve_kind(ctx, &space, selection).await?;
            let context = super::items::space_context(ctx, space).await?;
            Ok((context, resolved))
        }
        checkout => Ok((checkout, selection)),
    }
}

/// The items `selection` names: in the workspace, where it may name linked
/// ones, or in the repository of the registered checkout. With `elsewhere`,
/// an issue, PR or CI run URL of another repository selects that item.
pub(super) async fn select_items(
    ctx: &Context,
    context: &ItemContext,
    selection: Selection,
    elsewhere: bool,
) -> Result<Selected> {
    match context {
        ItemContext::Workspace(space) | ItemContext::Chat(space) => {
            let method = ItemMethod::SelectItems {
                space: space.clone(),
                selection,
                elsewhere,
            };
            client::request(&ctx.paths, method).await
        }
        ItemContext::Checkout(repository) => {
            checkout_selected(ctx, repository, selection, elsewhere).await
        }
    }
}

/// The item `selection` names in the repository of its registered checkout,
/// where nothing is linked, or with `elsewhere` in the repository its issue,
/// PR or CI run URL names.
async fn checkout_selected(
    ctx: &Context,
    repository: &str,
    selection: Selection,
    elsewhere: bool,
) -> Result<Selected> {
    let input = selection
        .input
        .context("outside a workspace or chat, nothing is linked; pass a number or URL")?;
    let repo = super::items::checkout_repository(ctx, repository).await?;
    let forge = match elsewhere {
        true => item_url_forge(ctx, selection.kind, &input).await?,
        false => None,
    };
    let forge = match forge {
        Some(forge) => forge,
        None => super::issues::origin_forge(ctx, &repo).await?,
    };
    let kind = match selection.kind {
        Some(kind) => kind,
        None => forge
            .item_kind(&ctx.forge_account()?, input.parse()?)
            .await?
            .into(),
    };
    let (_, url) = match kind {
        LinkKind::Issue => forge.issue(&input)?,
        LinkKind::Pr => forge.pull(&input)?,
        LinkKind::Ci => forge.run(&input)?,
    };
    Ok(Selected {
        path: repo.path,
        items: vec![SelectedItem { url, kind }],
    })
}

/// The repository of the issue, PR or CI run whose URL `input` is.
async fn item_url_forge(
    ctx: &Context,
    kind: Option<LinkKind>,
    input: &str,
) -> Result<Option<ForgeRepo>> {
    if IssueInput::parse(input) != IssueInput::Url {
        return Ok(None);
    }
    let hosts = ctx.forge_hosts()?;
    Ok(Some(match kind {
        Some(LinkKind::Issue) => ForgeRepo::resolve_issue_url(input, &hosts).await?,
        Some(LinkKind::Pr) => ForgeRepo::resolve_pull_url(input, &hosts).await?,
        Some(LinkKind::Ci) => ForgeRepo::resolve_run_url(input, &hosts).await?,
        None => return Ok(None),
    }))
}

/// Give a number selected without a kind the kind of the linked item with
/// that number, otherwise the kind the forge reports for it.
async fn resolve_kind(ctx: &Context, space: &str, selection: Selection) -> Result<Selection> {
    let (None, Some(input)) = (selection.kind, &selection.input) else {
        return Ok(selection);
    };
    let number = input.parse::<u64>()?;
    let kind = match linked_kind(ctx, space, number).await? {
        Some(kind) => kind,
        None => {
            let repo = space_repository(ctx, space).await?;
            super::issues::origin_forge(ctx, &repo)
                .await?
                .item_kind(&ctx.forge_account()?, number)
                .await?
                .into()
        }
    };
    Ok(Selection {
        kind: Some(kind),
        ..selection
    })
}

/// The kind of the space's item numbered `number` under any relation.
async fn linked_kind(ctx: &Context, space: &str, number: u64) -> Result<Option<LinkKind>> {
    let mut linked: Selected = client::request(
        &ctx.paths,
        ItemMethod::SelectItems {
            space: space.to_owned(),
            selection: Selection::default(),
            elsewhere: false,
        },
    )
    .await?;
    let links = match client::present_space(&ctx.paths, space).await? {
        Some(PresentSpace::Workspace(workspace)) => workspace.links,
        Some(PresentSpace::Chat(chat)) => chat.links,
        None => Default::default(),
    };
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
        "link needs an issue or PR number, or an issue, PR or CI run URL"
    );
    let space = ui::select_space(ctx, items.space, ui::Fallback::CurrentDirectory).await?;
    let selected = resolve_kind(ctx, &space, selected).await?;
    let (kind, input) = match selected {
        Selection {
            kind: Some(kind),
            input: Some(input),
        } => (kind, input),
        Selection { kind, .. } => pick_open_item(ctx, &space, kind).await?,
    };
    let title = match kind {
        LinkKind::Issue => issue_title(ctx, &space, &input).await,
        LinkKind::Pr | LinkKind::Ci => None,
    };
    client::request::<()>(
        &ctx.paths,
        ItemMethod::LinkItem {
            space,
            kind,
            url: input,
            relation,
            title,
        },
    )
    .await?;
    ctx.emit(
        &link_outcome(kind, relation),
        Registered {
            registered: true,
            relation: Some(relation),
        },
    )?;
    Ok(0)
}

fn link_outcome(kind: LinkKind, relation: Relation) -> String {
    match (relation, kind) {
        (_, LinkKind::Ci) => {
            format!("CI run linked as {relation}; shoal watch reports when it finishes.")
        }
        (Relation::Assigned, LinkKind::Pr) => {
            "PR linked as assigned; run shoal watch pr for updates and shoal done once the assignment is finished.".into()
        }
        (Relation::Assigned, LinkKind::Issue) => {
            "Issue linked as assigned; closure can complete the assignment.".into()
        }
        (Relation::Opened, LinkKind::Pr) => "PR linked as opened.".into(),
        (Relation::Opened, LinkKind::Issue) => "Issue linked as opened.".into(),
        (Relation::Related, LinkKind::Pr) => "PR linked as related.".into(),
        (Relation::Related, LinkKind::Issue) => "Issue linked as related.".into(),
    }
}

/// The title lists show for the space; linking proceeds without one when
/// the forge cannot be read.
async fn issue_title(ctx: &Context, space: &str, input: &str) -> Option<String> {
    let title = async {
        let repo = space_repository(ctx, space).await?;
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

/// The registered repository of the workspace or running chat `space` names.
async fn space_repository(ctx: &Context, space: &str) -> Result<Repository> {
    let repository = match client::present_space(&ctx.paths, space).await? {
        Some(PresentSpace::Workspace(workspace)) => workspace.repository_id,
        Some(PresentSpace::Chat(chat)) => chat.repository_id,
        None => {
            let unknown = format!("no workspace or running chat {space}");
            return Err(Refusal::new(Reason::NotFound, unknown).into());
        }
    };
    client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == repository)
        .context("the space's repository is missing")
}

/// Choose an open issue or PR of the space's repository, asking for the
/// kind when it is not given; runs have no list to choose from.
async fn pick_open_item(
    ctx: &Context,
    space: &str,
    kind: Option<LinkKind>,
) -> Result<(LinkKind, String)> {
    let kind = match kind {
        Some(LinkKind::Ci) => bail!("link ci needs a run's number or URL"),
        kind => kind.and_then(LinkKind::item),
    };
    let kind = match kind {
        Some(kind) => kind,
        None => ui::pick_choice(
            ctx,
            "Link> ",
            &[(ItemKind::Issue, "Issue"), (ItemKind::Pr, "Pull request")],
        )?,
    };
    let repo = space_repository(ctx, space).await?;
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
    Ok((kind.into(), number))
}

pub(super) async fn unlink(ctx: &Context, items: RelationArgs) -> Result<i32> {
    let (items, relation) = items.split()?;
    let (space, selected) = select(ctx, items).await?;
    client::request::<()>(
        &ctx.paths,
        ItemMethod::UnlinkItems {
            space,
            relation,
            kind: selected.kind,
            url: selected.input,
        },
    )
    .await?;
    let unlinked = Registered {
        registered: false,
        relation: None,
    };
    ctx.emit("Items unlinked", unlinked)?;
    Ok(0)
}

pub(super) async fn watch(
    ctx: &Context,
    items: ItemArgs,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    let (space, selected) = select(ctx, items).await?;
    workspaces::watch_items(ctx, space, selected, timeout, resumed).await
}

pub(super) async fn ready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let (space, selection) = select(ctx, items).await?;
    let itself = space_kind(ctx, &space).await?;
    let marks: Vec<ReviewMark> =
        client::request(&ctx.paths, AssignmentMethod::MarkReady { space, selection }).await?;
    for warning in marks.iter().flat_map(|mark| &mark.warnings) {
        eprintln!("{warning}");
    }
    ctx.emit(
        &format!("Ready for review:\n{}", review_lines(&marks, itself)),
        &marks,
    )?;
    Ok(0)
}

/// `draft` overrides `[pr] draft_on_unready` when given.
pub(super) async fn unready(ctx: &Context, items: ItemArgs, draft: Option<bool>) -> Result<i32> {
    let (space, selection) = select(ctx, items).await?;
    let itself = space_kind(ctx, &space).await?;
    let marks: Vec<ReviewMark> = client::request(
        &ctx.paths,
        AssignmentMethod::ClearReady {
            space,
            selection,
            draft,
        },
    )
    .await?;
    let message = format!(
        "Ready-for-review marks cleared:\n{}",
        review_lines(&marks, itself)
    );
    ctx.emit(&message, &marks)?;
    Ok(0)
}

/// What a mark on the space `space` itself is called: `workspace` or
/// `chat`.
async fn space_kind(ctx: &Context, space: &str) -> Result<&'static str> {
    Ok(match client::present_space(&ctx.paths, space).await? {
        Some(PresentSpace::Chat(_)) => "chat",
        _ => "workspace",
    })
}

/// One indented line per mark: its item, or the space itself, called
/// `itself`, and whether new commits arrived since.
pub(super) fn review_lines(marks: &[ReviewMark], itself: &str) -> String {
    marks
        .iter()
        .map(|mark| {
            let item = match (mark.kind, &mark.url) {
                (Some(kind), Some(url)) => format!("{kind} {url}"),
                _ => itself.to_owned(),
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
        LinkTarget::Run => bail!("a CI run names no work; use an issue, PR or branch link"),
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
