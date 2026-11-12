//! `--permission` on `shoal resume`: a change to the permissions the restored
//! agents and chats and the agents waiting for failed setups were given. A
//! scoped process gives only what it holds, which is checked for every
//! selected space before anything is set up or restores, and a resume makes
//! the change in the records it restores or starts once nothing refuses it.
use anyhow::{Context as _, Result, ensure};

use super::{setups::FailedSetup, targets::Selected};
use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::context::Context,
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::{Chat, Workspace},
    protocol::SavedAgent,
};

/// Refuse `change` for the stopped agents and last sessions of the
/// `selected` workspaces, only `execution`'s when given, the selected chats
/// and the agents waiting for the selected failed setups, where this
/// process does not hold what it gives or one's waiting wrapper restores
/// it, which keeps the permissions it launched with. It writes nothing: a
/// resume makes the change with [`apply`] once nothing refuses it.
pub(super) async fn check(
    ctx: &Context,
    selected: &Selected,
    execution: Option<&ExecutionId>,
    change: &PermissionChange,
) -> Result<()> {
    let selectors = (selected.workspaces.as_deref())
        .context("--permission changes the permissions of the spaces a resume names")?;
    for selector in selectors {
        let (workspace, saved) = saved_agents(ctx, selector, execution).await?;
        ensure!(
            !saved.agents.iter().any(|agent| agent.waits_in_wrapper),
            "the agent of {} restores in the wrapper that waits for it, which keeps its permissions; resume it without --permission",
            workspace.name
        );
        let repository = Some(workspace.repository_id.as_str().into());
        super::super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    for chat in &selected.chats {
        let repository = (chat.repository_id.as_ref()).map(|id| id.as_str().into());
        super::super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    for setup in selected
        .setups
        .iter()
        .filter(|setup| setup.launch.is_some())
    {
        let repository = Some(setup.workspace.repository_id.as_str().into());
        super::super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

/// The workspace `selector` names with the agents and last sessions it
/// saved, only `execution`'s when given.
async fn saved_agents(
    ctx: &Context,
    selector: &SpaceSelector,
    execution: Option<&ExecutionId>,
) -> Result<(Workspace, Saved)> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
        .await?
        .workspace;
    let mut saved =
        crate::client::saved::read(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, false);
    }
    Ok((workspace, saved))
}

/// Have the daemon make `change`, when given, to the permissions each agent
/// and last session the `stopped` workspaces keep, each of the stopped
/// `chats` and the agent waiting for each of the failed `setups` was given,
/// as their restores and setups begin, so one that fails keeps it too; it
/// changes none when it refuses one.
pub(super) async fn apply(
    ctx: &Context,
    stopped: &[(Workspace, Saved)],
    chats: &[Chat],
    setups: &[FailedSetup],
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agents = stopped.iter().flat_map(|(workspace, saved)| {
        (saved.agents.iter().chain(&saved.sessions)).map(|saved| SavedAgent {
            workspace: SpaceSelector::id(&workspace.id),
            execution: saved.id.clone(),
        })
    });
    let chats = chats.iter().map(|chat| SpaceSelector::id(&chat.id));
    let launches = (setups.iter())
        .filter(|setup| setup.launch.is_some())
        .map(|setup| SpaceSelector::id(&setup.workspace.id));
    let stopped = Stopped {
        agents: agents.collect(),
        chats: chats.collect(),
        launches: launches.collect(),
    };
    request(ctx, stopped, change).await
}

/// [`apply`] for the agent or last session `execution` of `workspace`.
pub(super) async fn apply_one(
    ctx: &Context,
    workspace: &Workspace,
    execution: &ExecutionId,
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agent = SavedAgent {
        workspace: SpaceSelector::id(&workspace.id),
        execution: execution.clone(),
    };
    let stopped = Stopped {
        agents: vec![agent],
        ..Stopped::default()
    };
    request(ctx, stopped, change).await
}

/// What a change to the permissions given applies to, as the daemon names
/// each.
#[derive(Default)]
struct Stopped {
    agents: Vec<SavedAgent>,
    chats: Vec<SpaceSelector>,
    /// The workspaces whose failed setup an agent waits for.
    launches: Vec<SpaceSelector>,
}

/// Have the daemon make `change`, when given, for the `stopped` agents,
/// chats and waiting launches.
async fn request(ctx: &Context, stopped: Stopped, change: Option<&PermissionChange>) -> Result<()> {
    let Stopped {
        agents,
        chats,
        launches,
    } = stopped;
    match change {
        Some(change) if !(agents.is_empty() && chats.is_empty() && launches.is_empty()) => {
            let change = change.clone();
            let paths = &ctx.paths;
            crate::client::assignments::change_stopped_permissions(
                paths, agents, chats, launches, change,
            )
            .await
        }
        _ => Ok(()),
    }
}
