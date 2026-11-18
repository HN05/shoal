//! Stopping spaces other joined machines run: each stop goes through the
//! daemon to that machine, for a space elsewhere and, unless skipped, the
//! spaces below it the hub lists, wherever they run, this machine's among
//! them, and for those below a space here; the hub holds a stop while its
//! machine is offline.
use std::collections::HashSet;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        directory::{HubView, Listed},
        routing::Queued,
    },
    space_names::SpaceSelector,
};

use crate::{
    cli::{commands::machines, context::Context, output::Style},
    client::output::agents::SpaceStopped,
    model::Space,
    protocol::{QueuedLaunches, UntrackedProcesses},
};

/// Stop the space `selected` names, when it is one other machine's space,
/// and the spaces below it unless `skip_children`; none when it is not.
pub(super) async fn stop(
    ctx: &Context,
    selected: &[SpaceSelector],
    skip_children: bool,
    yes: bool,
) -> Result<Option<i32>> {
    let [selector] = selected else {
        return Ok(None);
    };
    let Some((remote, view)) = machines::space_elsewhere(ctx, selector).await? else {
        return Ok(None);
    };
    stop_one(ctx, &remote, yes).await?;
    if !skip_children {
        let stopped = Stopped {
            here: Vec::new(),
            elsewhere: vec![remote],
        };
        stop_across(ctx, &view, stopped, yes).await?;
    }
    Ok(Some(0))
}

/// Stop the spaces below each of `stopped`, spaces of this machine that
/// stopped already, on every machine, as the hub lists them.
pub(super) async fn stop_below(ctx: &Context, stopped: &[Space], yes: bool) -> Result<()> {
    let Some(view) = machines::view(ctx).await else {
        return Ok(());
    };
    let stopped = Stopped {
        here: stopped.to_vec(),
        elsewhere: Vec::new(),
    };
    stop_across(ctx, &view, stopped, yes).await
}

/// The spaces a stop stopped last, here and on other machines.
struct Stopped {
    here: Vec<Space>,
    elsewhere: Vec<Listed>,
}

/// Stop the spaces below `stopped` on other machines and those below them
/// back here, crossing between machines as often as the tree does, each
/// once: this machine's below one of its own it stopped already.
async fn stop_across(ctx: &Context, view: &HubView, mut stopped: Stopped, yes: bool) -> Result<()> {
    let mut seen: HashSet<SpaceId> = (stopped.here.iter().map(|space| space.id.clone()))
        .chain(
            stopped
                .elsewhere
                .iter()
                .map(|listed| listed.space.id.clone()),
        )
        .collect();
    loop {
        let mut elsewhere = Vec::new();
        let parents = (stopped.here.iter().map(|space| &space.id))
            .chain(stopped.elsewhere.iter().map(|listed| &listed.space.id));
        for parent in parents.collect::<Vec<_>>() {
            for listed in below(view, parent) {
                if listed.machine != view.machine && seen.insert(listed.space.id.clone()) {
                    stop_one(ctx, &listed, yes).await?;
                    elsewhere.push(listed);
                }
            }
        }
        let back_here: Vec<SpaceSelector> = (view.below_elsewhere.iter())
            .filter(|(_, parent)| {
                elsewhere
                    .iter()
                    .chain(&stopped.elsewhere)
                    .any(|l| &l.space.id == parent)
            })
            .filter(|(space, _)| !seen.contains(space))
            .map(|(space, _)| SpaceSelector::id(space))
            .collect();
        let mut here = Vec::new();
        if !back_here.is_empty() {
            for space in super::resolve(ctx, back_here, true).await? {
                if seen.insert(space.id.clone()) {
                    super::stop_one(ctx, &space, yes).await?;
                    here.push(space);
                }
            }
        }
        if here.is_empty() && elsewhere.is_empty() {
            return Ok(());
        }
        stopped = Stopped { here, elsewhere };
    }
}

/// Stop `space` on its machine, reporting it, or that the hub holds the
/// stop.
async fn stop_one(ctx: &Context, space: &Listed, yes: bool) -> Result<()> {
    let selector = SpaceSelector::id(&space.space.id);
    // A process here is none of that machine's.
    let processes = UntrackedProcesses::Stop { requester: None };
    let stopped = super::stop(ctx, &selector, yes, QueuedLaunches::Cancel, processes).await;
    let stopped = match stopped {
        Ok(stopped) => stopped,
        Err(error) => match error.downcast::<Queued>() {
            Ok(queued) => return machines::queued(ctx, &queued).map(drop),
            Err(error) => return Err(error),
        },
    };
    let message = format!("Stopped {} on its machine", space.space.name);
    let report = SpaceStopped {
        stopped: true,
        saved: stopped.saved,
        other_processes: stopped.other_processes,
        cancelled: stopped.cancelled,
        cancelled_run: stopped.cancelled_run,
    };
    ctx.emit_styled(Style::Success, &message, report)
}

/// The spaces below `id` that `view` lists, on any machine, each above
/// those below it.
fn below(view: &HubView, id: &SpaceId) -> Vec<Listed> {
    let mut found: Vec<Listed> = Vec::new();
    let mut parents = vec![id.clone()];
    while let Some(parent) = parents.pop() {
        let children = (view.directory.spaces.iter())
            .filter(|listed| listed.space.parent.as_ref() == Some(&parent))
            .filter(|listed| !found.iter().any(|known| known.space.id == listed.space.id));
        for child in children.cloned().collect::<Vec<_>>() {
            parents.push(child.space.id.clone());
            found.push(child);
        }
    }
    found
}

#[cfg(test)]
mod tests;
