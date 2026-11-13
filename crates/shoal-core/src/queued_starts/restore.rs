//! An admitted restore: `shoal restore` of its stopped agent runs where the
//! agent stopped when that place is back at its prompt, otherwise in a
//! background place, and takes over the entry's slot.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::client::{execution_ids::ExecutionId, space_names::SpaceSelector};
use crate::{
    agents::HomeRecords,
    config::Effective,
    model::{PresentSpace, QueueEntry},
    paths::CommandPaths,
    protocol::ConfigTarget,
    sessions::Origin,
    terminal::queued,
};

/// Restore the stopped agent the admitted restore `entry` names.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntry) -> Result<()> {
    let (Some(space), Some(execution)) = (&entry.space_id, &entry.execution) else {
        anyhow::bail!("the queued restore names no stopped agent");
    };
    let selector = SpaceSelector::id(space);
    let space = crate::client::workspaces::present_space(paths, &selector)
        .await?
        .with_context(|| format!("no space {selector}"))?;
    let settings = crate::config::settings(paths, ConfigTarget::Space(selector.clone())).await?;
    let stopped = stopped_place(paths, selector, execution).await?;
    let args = restore_args(&space, execution, entry);
    if queued::rerun_where_stopped(paths, &settings, stopped.as_ref(), &args).await? {
        return Ok(());
    }
    match &space {
        PresentSpace::Workspace(workspace) => {
            queued::restore_in_background(paths, &settings, workspace, &args).await
        }
        PresentSpace::Chat(_) => start_chat(paths, &settings, &space, &args).await,
    }
}

/// The place where `execution`, a stopped agent of the space `selector`
/// names, stopped, as its saved session records it.
async fn stopped_place(
    paths: &CommandPaths,
    selector: SpaceSelector,
    execution: &ExecutionId,
) -> Result<Option<Origin>> {
    let saved = crate::client::saved::read(paths, selector).await?;
    Ok(saved
        .session(execution)
        .and_then(|session| session.origin.clone()))
}

/// The `shoal` arguments that restore `execution`, the stopped agent of
/// `space`, for the admitted `entry`, whose slot it takes over.
fn restore_args(
    space: &PresentSpace,
    execution: &ExecutionId,
    entry: &QueueEntry,
) -> [OsString; 7] {
    [
        "restore",
        space.id().as_str(),
        "--execution",
        execution.as_str(),
        "--focus",
        "--queue-entry",
        entry.id.as_str(),
    ]
    .map(OsString::from)
}

/// Run Shoal with `args`, a restore of the agent of the chat `space`, in a
/// new background place of the first host that would start a queued agent,
/// in its home.
pub async fn start_chat(
    paths: &CommandPaths,
    settings: &Effective,
    space: &PresentSpace,
    args: &[OsString],
) -> Result<()> {
    let records = HomeRecords::read(paths, &SpaceSelector::id(space.id())).await?;
    let home = records.home()?;
    let repository = match &records {
        HomeRecords::Chat { repository, .. } => repository.as_ref(),
        HomeRecords::Workspace(_) => None,
    };
    let name = space.name().as_str();
    let argv = crate::terminal::shoal_command(paths, args)?;
    queued::start_chat(paths, settings, repository, home.path(), name, argv, true).await?;
    Ok(())
}
