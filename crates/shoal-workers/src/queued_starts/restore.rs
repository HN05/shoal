//! An admitted restore: `shoal restore` of its stopped agent runs where the
//! agent stopped when that place is back at its prompt, otherwise in a
//! background place, and takes over the entry's slot.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    client::connection::CommandPaths,
    model::{QueueEntry, Space},
    places::queued,
    protocol::ConfigTarget,
    sessions::Origin,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

/// Restore the stopped agent the admitted restore `entry` names.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntry) -> Result<()> {
    let (Some(space), Some(execution)) = (&entry.space_id, &entry.execution) else {
        anyhow::bail!("the queued restore names no stopped agent");
    };
    let selector = SpaceSelector::id(space);
    let space = crate::client::workspaces::present_space(paths, &selector)
        .await?
        .with_context(|| format!("no space {selector}"))?;
    let settings = crate::settings::of(paths, ConfigTarget::Space(selector.clone())).await?;
    let stopped = stopped_place(paths, selector, execution).await?;
    let args = restore_args(&space, execution, entry);
    if crate::terminal::queued::rerun_where_stopped(paths, &settings, stopped.as_ref(), &args)
        .await?
    {
        return Ok(());
    }
    match space.to_workspace() {
        Some(workspace) => queued::restore_in_background(paths, &settings, &workspace, &args).await,
        None => queued::restore_chat(paths, &settings, &space, &args).await,
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
fn restore_args(space: &Space, execution: &ExecutionId, entry: &QueueEntry) -> [OsString; 7] {
    [
        "restore",
        space.id.as_str(),
        "--execution",
        execution.as_str(),
        "--focus",
        "--queue-entry",
        entry.id.as_str(),
    ]
    .map(OsString::from)
}
