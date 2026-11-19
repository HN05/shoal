//! What a start that runs in this caller settles before anything is
//! created, whether or not a terminal then picks the rest: its repository,
//! what its permissions and named agent would refuse, and what the daemon
//! would refuse of its new workspace, which names the workspace whose
//! branch it names.
use anyhow::Result;

use super::{
    agent::AddSettings,
    target::{AddTarget, resolve_add_target},
};
use crate::cli::{commands::issues, context::Context};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};

/// What a start that runs in this caller settled: where it starts, the
/// settings of the repository, or of the workspace whose branch it names,
/// and whether that workspace's own issue prompts its agent.
pub(super) struct Resolved<'a> {
    pub target: AddTarget,
    pub settings: AddSettings<'a>,
    pub issue_linked: bool,
}

/// Settle `launch` as [`Resolved`] says, naming its repository by ID,
/// refusing what the start cannot take before a picker asks anything.
pub(super) async fn resolve<'a>(ctx: &'a Context, launch: &mut Launch) -> Result<Resolved<'a>> {
    let target = start_target(ctx, launch).await?;
    let selector = Some(target.selector.clone());
    crate::cli::commands::permissions::ensure_held(ctx, selector, &launch.permissions).await?;
    // The daemon settles the launch as its start would, short of creating
    // or fetching anything; a branch a workspace has reopens it, under its
    // own settings.
    let reopened = crate::client::launches::check(&ctx.paths, launch.clone()).await?;
    let issue_linked =
        (reopened.as_ref()).is_some_and(|workspace| workspace.links.issue().is_some());
    let settings = match &reopened {
        Some(workspace) => AddSettings::workspace(ctx, &workspace.id),
        None => AddSettings::new(ctx, &target.selector),
    };
    ensure_agent_runs(&settings, launch).await?;
    Ok(Resolved {
        target,
        settings,
        issue_linked,
    })
}

/// Refuse what `launch` asks for that its arguments alone refuse, before
/// anything is looked up or a picker opens: two repositories, an issue to
/// pick outside plan mode, a link beside a branch, an issue or a copy, and
/// a branch that is not a literal Git branch name.
pub(in crate::cli::commands) fn check_start_arguments(launch: &Launch) -> Result<()> {
    // A number beside a branch or issue of the start's own names a
    // repository rather than an item.
    let positional = crate::launches::decided_target(launch)?;
    anyhow::ensure!(
        launch.repository.is_none() || !matches!(positional, Some(StartTarget::Repository(_))),
        "a positional repository cannot be combined with --repo"
    );
    // Only plan mode, which starts in a chat, picks the issue.
    anyhow::ensure!(
        !matches!(launch.issue, Some(IssueChoice::Pick)),
        "--issue needs a number or URL outside plan mode"
    );
    let links = !matches!(positional, None | Some(StartTarget::Repository(_)));
    anyhow::ensure!(
        !links || (launch.branch.is_none() && launch.issue.is_none()),
        "an item or branch link cannot be combined with --branch or --issue"
    );
    crate::launches::ensure_copy_fits(launch)?;
    if let Some(branch) = &launch.branch {
        shoal_core::git::check_branch_name(branch)?;
    }
    Ok(())
}

/// The repository `launch` starts in: the one it names, else the one its
/// link belongs to, offering to register it, else the one
/// [`resolve_add_target`] finds; `launch` then names it by ID.
async fn start_target(ctx: &Context, launch: &mut Launch) -> Result<AddTarget> {
    check_start_arguments(launch)?;
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let issue = launch.issue.as_ref().and_then(IssueChoice::named).cloned();
    let named = match named {
        Some(named) => Some(crate::repository::named(named)?),
        None => linked_repository(ctx, launch).await?,
    };
    let parent = launch.parent.as_ref();
    let target = resolve_add_target(ctx, named, issue.as_ref(), parent).await?;
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
