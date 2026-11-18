//! `--permission` on `shoal restore`: a change to the permissions the restored
//! agents and chats, the agents waiting for failed setups and the failed
//! launches restored were given. A
//! scoped process gives only what it holds, which is checked for every
//! selected space before anything is set up or restores, and a restore makes
//! the change in the records it restores or starts once nothing refuses it.
use anyhow::{Context as _, Result};

use super::super::targets::Selected;
use super::setups::FailedSetup;
use crate::{
    cli::context::Context,
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::{QueueEntry, Space},
    protocol::SavedAgent,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Refuse `change` for the stopped agents and last sessions of the
/// `selected` workspaces, the selected chats and the agents waiting for the
/// selected failed setups, where this process does not hold what it gives. It writes nothing: a resume makes
/// the change with [`apply`] once nothing refuses it.
pub(super) async fn check(
    ctx: &Context,
    selected: &Selected,
    change: &PermissionChange,
) -> Result<()> {
    let selectors = (selected.spaces.as_deref())
        .context("--permission changes the permissions of the spaces a restore names")?;
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let repository = space.repository_id.as_ref().map(|id| id.as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    for setup in selected
        .setups
        .iter()
        .filter(|setup| setup.launch.is_some())
    {
        let repository = Some(setup.workspace.repository().as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    for failed in &selected.failed_starts {
        let repository = failed.repository_id.as_ref().map(|id| id.as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

/// Have the daemon make `change`, when given, to the permissions each agent
/// and last session the `stopped` spaces keep, the agent waiting for each
/// of the failed `setups` and each of the `failed` launches was given,
/// as their restores and setups begin, so one that fails keeps it too; it
/// changes none when it refuses one.
pub(super) async fn apply(
    ctx: &Context,
    stopped: &[(Space, Saved)],
    (setups, failed): (&[FailedSetup], &[QueueEntry]),
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agents = stopped.iter().flat_map(|(space, saved)| {
        (saved.agents.iter().chain(&saved.sessions)).map(|saved| SavedAgent {
            workspace: SpaceSelector::id(&space.id),
            execution: saved.id.clone(),
        })
    });
    let launches = (setups.iter())
        .filter(|setup| setup.launch.is_some())
        .map(|setup| SpaceSelector::id(&setup.workspace.id))
        .chain((failed.iter()).filter_map(|launch| Some(SpaceSelector::id(launch.space()?))));
    let targets = PermissionTargets {
        agents: agents.collect(),
        launches: launches.collect(),
    };
    request(ctx, targets, change).await
}

/// [`apply`] for the agent or last session `execution` of `space`.
pub(super) async fn apply_one(
    ctx: &Context,
    space: &Space,
    execution: &ExecutionId,
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agent = SavedAgent {
        workspace: SpaceSelector::id(&space.id),
        execution: execution.clone(),
    };
    let targets = PermissionTargets {
        agents: vec![agent],
        ..PermissionTargets::default()
    };
    request(ctx, targets, change).await
}

/// What a change to the permissions given applies to, as the daemon names
/// each.
#[derive(Default)]
struct PermissionTargets {
    agents: Vec<SavedAgent>,
    /// The workspaces whose failed setup an agent waits for, and the
    /// spaces of failed launches.
    launches: Vec<SpaceSelector>,
}

/// Have the daemon make `change`, when given, for the `targets` agents and
/// waiting launches.
async fn request(
    ctx: &Context,
    targets: PermissionTargets,
    change: Option<&PermissionChange>,
) -> Result<()> {
    let PermissionTargets { agents, launches } = targets;
    match change {
        Some(change) if !(agents.is_empty() && launches.is_empty()) => {
            let change = change.clone();
            let paths = &ctx.paths;
            crate::client::assignments::change_stopped_permissions(paths, agents, launches, change)
                .await
        }
        _ => Ok(()),
    }
}
