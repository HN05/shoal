//! What an addition creates: the [`AddPlan`] a terminal host or the agent
//! queue's worker carries out, built from the command's arguments.
use std::ffi::OsString;

use anyhow::Result;

use super::{AddOptions, reads::DaemonReads, target::AddTarget};
use crate::{
    cli::{
        commands::{issues, repositories},
        context::Context,
    },
    env,
    launches::workspace,
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    terminal::PlaceName,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

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

/// The workspace name `creation` gives, else the one it will derive, as
/// [`workspace::planned_name`] names it.
pub(super) async fn planned_workspace_name(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<SpaceName> {
    workspace::planned_name(target.repository(ctx).await?, creation).await
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
    let repo = target.repository(ctx).await?;
    workspace::reopened_workspace(&DaemonReads(ctx), repo, &plan.creation).await
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
