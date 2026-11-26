//! A resume's change to the permissions of what it resumes.
use anyhow::Result;

use crate::{
    client::connection::CommandPaths,
    config::permissions::PermissionChange,
    model::{QueueEntry, Space},
    protocol::SavedAgent,
};
use shoal_core::execution::recovery::Saved;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Have the daemon make `change`, when given, to the permissions of each
/// agent and last session the `stopped` spaces keep, and of each `failed`
/// launch. It makes the change as their restores begin, so one that fails
/// keeps it too. When it refuses one, it changes none.
pub async fn apply(
    paths: &CommandPaths,
    stopped: &[(Space, Saved)],
    failed: &[QueueEntry],
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agents = stopped.iter().flat_map(|(space, saved)| {
        (saved.agents.iter().chain(&saved.sessions)).map(|saved| SavedAgent {
            workspace: SpaceSelector::id(&space.id),
            execution: saved.id.clone(),
        })
    });
    let launches = (failed.iter()).filter_map(|launch| Some(SpaceSelector::id(launch.space()?)));
    let targets = PermissionTargets {
        agents: agents.collect(),
        launches: launches.collect(),
    };
    request(paths, targets, change).await
}

/// [`apply`] for the agent or last session `execution` of `space`.
pub async fn apply_one(
    paths: &CommandPaths,
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
    request(paths, targets, change).await
}

/// What a change to the permissions given applies to, as the daemon names
/// each.
#[derive(Default)]
struct PermissionTargets {
    agents: Vec<SavedAgent>,
    /// The spaces of failed launches.
    launches: Vec<SpaceSelector>,
}

/// Have the daemon make `change`, when given, for the `targets` agents and
/// waiting launches.
async fn request(
    paths: &CommandPaths,
    targets: PermissionTargets,
    change: Option<&PermissionChange>,
) -> Result<()> {
    let PermissionTargets { agents, launches } = targets;
    match change {
        Some(change) if !(agents.is_empty() && launches.is_empty()) => {
            let change = change.clone();
            crate::client::assignments::change_stopped_permissions(paths, agents, launches, change)
                .await
        }
        _ => Ok(()),
    }
}
