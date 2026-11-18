//! What a start that runs in this caller settles before anything is
//! created, whether or not a terminal then picks the rest: its repository,
//! what creation and its named agent, blockers and permissions would
//! refuse, the workspace whose branch it names, and the issue or PR it
//! starts on, read so that a lookup's failure ends the start first.
use anyhow::Result;

use super::{
    agent::AddSettings,
    launch::issue_default,
    plan::check_add_creation,
    reads::DaemonReads,
    target::{AddTarget, resolve_add_target},
};
use crate::{
    cli::{client, commands::issues, context::Context},
    model::{Blocker, Workspace},
    start_plan::Creation,
};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};

/// What a start that runs in this caller settled: where it starts, the
/// settings of the repository, or of the workspace whose branch it names,
/// whether that workspace's own issue prompts its agent, and whether it
/// opens a fork PR's head, which holds no agent.
pub(super) struct Resolved<'a> {
    pub target: AddTarget,
    pub settings: AddSettings<'a>,
    pub issue_linked: bool,
    pub fork_head: bool,
}

/// Settle `launch` as [`Resolved`] says, naming its repository by ID,
/// refusing what the start cannot take before a picker asks anything.
pub(super) async fn resolve<'a>(ctx: &'a Context, launch: &mut Launch) -> Result<Resolved<'a>> {
    let target = start_target(ctx, launch).await?;
    let selector = Some(target.selector.clone());
    crate::cli::commands::permissions::ensure_held(ctx, selector, &launch.permissions).await?;
    check_named_blockers(ctx, &target, &launch.blockers).await?;
    // A branch a workspace has reopens it, under its own settings.
    let reopened = check_launch_creation(ctx, &target, launch).await?;
    let issue_linked =
        (reopened.as_ref()).is_some_and(|workspace| workspace.links.issue().is_some());
    let settings = match &reopened {
        Some(workspace) => AddSettings::workspace(ctx, &workspace.id),
        None => AddSettings::new(ctx, &target.selector),
    };
    ensure_agent_runs(&settings, launch).await?;
    let fork_head = read_start_item(ctx, &target, launch).await?;
    Ok(Resolved {
        target,
        settings,
        issue_linked,
        fork_head,
    })
}

/// The repository `launch` starts in: the one it names, else the one its
/// link belongs to, offering to register it, else the current or a picked
/// one; `launch` then names it by ID.
async fn start_target(ctx: &Context, launch: &mut Launch) -> Result<AddTarget> {
    anyhow::ensure!(
        launch.repository.is_none() || !matches!(launch.target, Some(StartTarget::Repository(_))),
        "a positional repository cannot be combined with --repo"
    );
    let links = !matches!(launch.target, None | Some(StartTarget::Repository(_)));
    anyhow::ensure!(
        !links || (launch.branch.is_none() && launch.issue.is_none()),
        "an item or branch link cannot be combined with --branch or --issue"
    );
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let issue = launch.issue.as_ref().and_then(IssueChoice::named).cloned();
    let named = match named {
        Some(named) => Some(crate::repository::named(named)?),
        None => linked_repository(ctx, launch).await?,
    };
    let target = resolve_add_target(ctx, named, issue.as_ref()).await?;
    let repo = target.repository(ctx).await?;
    // A PR or branch link opens only in the repository it belongs to.
    if matches!(
        launch.target,
        Some(StartTarget::Pull(_) | StartTarget::Branch { .. })
    ) {
        let linked = (launch.target.as_ref()).and_then(StartTarget::linked_repository);
        crate::launches::workspace::ensure_link_in(repo, linked).await?;
    }
    launch.repository = Some(repo.id.to_string());
    if matches!(launch.target, Some(StartTarget::Repository(_))) {
        launch.target = None;
    }
    Ok(target)
}

/// Read the issue or PR `launch` starts on in `target`'s repository, so
/// that one that cannot be read ends the start before a picker asks,
/// returning whether it opens a fork PR's head, which holds no agent and
/// so refuses one named.
async fn read_start_item(ctx: &Context, target: &AddTarget, launch: &Launch) -> Result<bool> {
    if let Some(issue) = named_issue(launch)? {
        let context = issues::read_context(ctx, target.repository(ctx).await?).await?;
        issues::load(ctx, context, issue).await?;
    }
    let Some(StartTarget::Pull(pull)) = &launch.target else {
        return Ok(false);
    };
    let context = issues::read_context(ctx, target.repository(ctx).await?).await?;
    let pull = client::pull_request(&ctx.paths, context, pull.clone().into()).await?;
    anyhow::ensure!(
        !pull.fork || launch.agent.is_none(),
        "the PR comes from a fork, so its head opens in a workspace at a commit, which holds no agent of its own"
    );
    Ok(pull.fork)
}

/// The issue `launch` starts on, by `--issue` or the positional argument.
fn named_issue(launch: &Launch) -> Result<Option<crate::forge::items::reference::ItemInput>> {
    if let Some(issue) = launch.issue.as_ref().and_then(IssueChoice::named) {
        return Ok(Some(issue.clone()));
    }
    Ok(
        match (launch.target.clone())
            .map(|target| target.decided(false))
            .transpose()?
        {
            Some(StartTarget::Issue(issue)) if issue_default(launch) => Some(issue),
            _ => None,
        },
    )
}

/// Refuse the workspaces, queued agents and issues a start `named` as its
/// blockers in `target`'s repository when the daemon knows none of them,
/// before a picker asks anything.
pub(super) async fn check_named_blockers(
    ctx: &Context,
    target: &AddTarget,
    named: &[String],
) -> Result<()> {
    if named.is_empty() {
        return Ok(());
    }
    let repo = target.repository(ctx).await?;
    let mut blockers = Vec::new();
    for input in named {
        blockers.push(match input.parse()? {
            Blocker::Issue(issue) => Blocker::Issue(issues::issue(ctx, repo, &issue).await?.into()),
            blocker => blocker,
        });
    }
    let selector = target.selector.clone();
    crate::client::workspaces::check_blockers(&ctx.paths, selector, blockers).await
}

/// Refuse the agent `launch` names, and the model it asks of it, before a
/// picker asks anything, when `settings` cannot start them.
async fn ensure_agent_runs(settings: &AddSettings<'_>, launch: &Launch) -> Result<()> {
    let Some(agent) = &launch.agent else {
        return Ok(());
    };
    let settings = settings.get().await?;
    crate::agents::ensure_available(agent, settings)?;
    let model = crate::agents::resolve_model(settings, agent, launch.model.clone());
    crate::agents::check_model(settings, agent, &model)
}

/// Refuse, before a picker asks anything, what creating the workspace
/// `launch` names in `target` would refuse, returning instead the
/// workspace that already has the branch it names, which the start opens.
async fn check_launch_creation(
    ctx: &Context,
    target: &AddTarget,
    launch: &Launch,
) -> Result<Option<Workspace>> {
    let mut creation = Creation {
        path: launch.path.clone(),
        branch: launch.branch.clone(),
        base: launch.base.clone(),
        git_profile: launch.git_profile.clone(),
        options: crate::protocol::CreationOptions {
            name: launch.name.clone(),
            priority: launch.priority,
            skip_setup: launch.skip_setup,
            disposable: launch.disposable,
        },
        ..Creation::default()
    };
    let repo = (&target.selector, target.repository(ctx).await?);
    let reads = DaemonReads(ctx);
    let settle = crate::launches::workspace::settle(&reads, repo, &mut creation);
    if let Some(workspace) = settle.await? {
        ensure_takes_existing(launch, &creation, &workspace)?;
        return Ok(Some(workspace));
    }
    check_add_creation(ctx, target, &creation, launch.parent.as_ref()).await?;
    Ok(None)
}

/// Refuse what only a new workspace takes for `launch`, which goes into the
/// existing `workspace` that has the branch it names, as the daemon refuses
/// it. The parent of a fork without a copy is the workspace whose agent it
/// forks, which an existing workspace's start takes too.
fn ensure_takes_existing(
    launch: &Launch,
    creation: &Creation,
    workspace: &Workspace,
) -> Result<()> {
    let forks_parent = launch.fork.is_some() && creation.copy.is_none();
    let parent = launch.parent.is_some() && !forks_parent;
    let issue = launch.issue.is_some();
    let Some(flag) = crate::launches::workspace::new_space_flag(creation, issue, parent) else {
        return Ok(());
    };
    let branch = workspace.branch().map_or("", |branch| branch.as_str());
    anyhow::bail!(
        "{flag} applies to a new workspace, and workspace {} already has branch {branch}; the start goes into it as --in does",
        workspace.number
    )
}

/// The registered repository the link `launch` starts on belongs to,
/// offering to register an unregistered one; none for a start on no link.
async fn linked_repository(
    ctx: &Context,
    launch: &Launch,
) -> Result<Option<crate::repository::Selector>> {
    let Some(linked) = (launch.target.as_ref()).and_then(StartTarget::linked_repository) else {
        return Ok(None);
    };
    let mut repos = crate::client::repositories::list(&ctx.paths).await?;
    let retry = "shoal start <link> --repo <repository>";
    let id = issues::registered_remote(ctx, &mut repos, linked.clone(), retry).await?;
    Ok(Some(id.into()))
}
