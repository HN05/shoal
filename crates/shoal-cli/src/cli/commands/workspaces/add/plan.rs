//! What an addition creates: the [`AddPlan`] a terminal host or the agent
//! queue's worker carries out, built from the command's arguments.
use std::ffi::OsString;

use anyhow::Result;

use super::{AddOptions, target::AddTarget};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{issues, repositories},
        context::Context,
    },
    env, git,
    model::Repository,
    protocol::ItemContext,
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    terminal::PlaceName,
};

/// `creation` with its path resolved against the current directory.
pub(super) fn absolute(ctx: &Context, mut creation: Creation) -> Result<Creation> {
    creation.path = creation
        .path
        .map(|path| repositories::absolute(ctx, path))
        .transpose()?;
    Ok(creation)
}

/// The plan to add `creation` in the chat this runs in, if any, starting
/// `agent` with the found `issue` or with `args`.
pub(super) async fn plan_addition(
    ctx: &Context,
    target: &AddTarget,
    mut creation: Creation,
    issue: Option<&issues::Issue>,
    agent: ResolvedAddAgent,
    args: Vec<OsString>,
) -> Result<AddPlan> {
    // Without a name of its own, the workspace takes the issue's title.
    if let Some(issue) = issue {
        (creation.options.name).get_or_insert_with(|| SpaceName::derive(&issue.title, "workspace"));
    }
    let place_name = match issue {
        Some(issue) => Some(PlaceName::new(
            None,
            target.repository(ctx).await?,
            Some(issue.place_issue()),
        )),
        None => None,
    };
    Ok(AddPlan {
        repository: target.selector.clone(),
        creation,
        place_name,
        issue: issue.map(|issue| issue.url.clone()),
        agent,
        args,
        prompt: None,
        mode: None,
        narrowing: Default::default(),
        fork: None,
        queue_entry: None,
        chat: env::in_chat(),
        parent: None,
        workspace: None,
    })
}

/// The workspace name `creation` gives, else the one it will derive. An
/// existing branch opens under its local name, so a remote's prefix is left
/// out, as opening it does.
pub(super) async fn planned_workspace_name(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<SpaceName> {
    if let Some(name) = &creation.options.name {
        return Ok(name.clone());
    }
    let branch = match (
        &creation.branch,
        opened_branch(ctx, target, creation).await?,
    ) {
        (Some(branch), _) => branch.clone(),
        (None, Some(local)) => local,
        (None, None) => (creation.copy.as_ref()).map_or_else(String::new, |copy| copy.name.clone()),
    };
    Ok(SpaceName::derive(&branch, "workspace"))
}

/// The local name of the existing branch `creation` opens, if any.
async fn opened_branch(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<Option<String>> {
    let Some(selector) = &creation.existing else {
        return Ok(None);
    };
    if let Some(local) = git::strip_local(selector) {
        return Ok(Some(local.to_owned()));
    }
    let repo = target.repository(ctx).await?;
    let remote = git::remote_branch(&repo.path, selector).await?;
    Ok(Some(
        remote.map_or_else(|| selector.clone(), |(_, branch)| branch),
    ))
}

/// The existing workspace `plan` reopens: the one it names, else the one on
/// the branch it opens. A launch there is that workspace's agent, which the
/// daemon admits only while the workspace has none.
pub(super) async fn reopened_workspace(
    ctx: &Context,
    target: &AddTarget,
    plan: &AddPlan,
) -> Result<Option<SpaceId>> {
    if plan.workspace.is_some() {
        return Ok(plan.workspace.clone());
    }
    let Some(branch) = opened_branch(ctx, target, &plan.creation).await? else {
        return Ok(None);
    };
    let repo = target.repository(ctx).await?;
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    Ok(workspaces
        .into_iter()
        .find(|workspace| {
            workspace.repository_id == repo.id && workspace.branch.as_deref() == Some(&branch)
        })
        .map(|workspace| workspace.id))
}

/// Agent arguments as a queued launch carries them, refusing one that is
/// not UTF-8.
pub(super) fn launch_args(args: Vec<OsString>) -> Result<Vec<String>> {
    (args.into_iter())
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("agent argument {arg:?} is not UTF-8"))
        })
        .collect()
}

/// Start `plan`'s agent in `mode` with the narrowing `options` gives,
/// below the space it names, from a fork of the session it names.
pub(super) fn start_as(
    plan: &mut AddPlan,
    options: &AddOptions,
    mode: Option<crate::config::modes::ModeName>,
) {
    plan.mode = mode;
    plan.narrowing = options.narrowing;
    plan.prompt.clone_from(&options.prompt);
    plan.parent.clone_from(&options.parent);
    plan.fork.clone_from(&options.fork);
}

/// Where `plan` reads from the forge: the existing workspace it launches in,
/// else the chat that planned it, else where a command reads from the forge
/// of its repository `repo`.
pub(in crate::cli) async fn item_context(
    ctx: &Context,
    plan: &AddPlan,
    repo: &Repository,
) -> Result<ItemContext> {
    Ok(match (&plan.workspace, &plan.chat) {
        (Some(workspace), _) => ItemContext::Workspace(SpaceSelector::id(workspace)),
        (None, Some(chat)) => ItemContext::Chat(SpaceSelector::id(chat)),
        (None, None) => issues::read_context(ctx, repo).await?,
    })
}

/// Refuse what creation would refuse in the given repository, branch and
/// options before a prompt asks for the rest of the addition. Only a terminal
/// prompts; without one, creation refuses the arguments itself, except that
/// a `parent`, else the chat this runs in, that the workspace cannot go below
/// is refused before a launch is queued.
pub(super) async fn check_add_creation(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
    parent: Option<&SpaceSelector>,
) -> Result<()> {
    let parent = (parent.cloned()).or_else(|| env::in_chat().map(SpaceSelector::Id));
    if !ctx.interactive() && parent.is_none() {
        return Ok(());
    }
    let planned = crate::client::workspaces::PlannedWorkspace {
        repository: target.selector.clone(),
        branch: creation.branch.clone(),
        existing: creation.existing.clone(),
        base: creation.base.clone(),
        git_profile: creation.git_profile.clone(),
        path: creation.path.clone(),
        parent,
        options: creation.options.clone(),
    };
    crate::client::workspaces::check_creation(&ctx.paths, planned).await
}
