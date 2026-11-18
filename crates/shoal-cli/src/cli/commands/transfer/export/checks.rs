//! What ending a space for its export would do, the refusals of an export
//! that would lose work or cut its own output off, and the confirmation
//! ending the spaces needs.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_protocol::model::ByKind;

use super::Output;
use crate::{
    cli::{context::Context, ui},
    model::{Chat, Space, SpaceHold, SpaceKind, Spaces},
    removal::{SpaceRemovalCheck, UnsavedWork},
    transfer::{Carrying, Placed},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// A space that passed its export check, with the spaces below it that go
/// with it, which its export ends too.
pub(in super::super) struct Checked {
    pub(in super::super) space: Ending,
    carried: Vec<Space>,
}

/// What ending a space for its export does: removing a workspace, with
/// what its removal would delete, or a chat.
#[allow(
    clippy::large_enum_variant,
    reason = "one per exported space, moved into its planned export"
)]
pub(in super::super) enum Ending {
    Workspace(SpaceRemovalCheck),
    Chat(Chat),
}

impl Checked {
    pub(super) fn name(&self) -> &shoal_protocol::space_names::SpaceName {
        match &self.space {
            Ending::Workspace(check) => &check.space.name,
            Ending::Chat(chat) => &chat.name,
        }
    }

    pub(in super::super) fn id(&self) -> &shoal_ids::space_ids::SpaceId {
        match &self.space {
            Ending::Workspace(check) => &check.space.id,
            Ending::Chat(chat) => &chat.id,
        }
    }

    /// The space with its kind, as a confirmation names it.
    pub(super) fn label(&self) -> String {
        match &self.space {
            Ending::Workspace(check) => format!("workspace {}", check.space.name),
            Ending::Chat(chat) => format!("chat {}", chat.name),
        }
    }

    /// The space's holds, which ending it here releases.
    fn own_holds(&self) -> &[SpaceHold] {
        match &self.space {
            Ending::Workspace(check) => &check.space.holds,
            Ending::Chat(chat) => &chat.holds,
        }
    }

    /// The space and the spaces that go with it, whose agents a move stops.
    pub(in super::super) fn spaces(&self) -> impl Iterator<Item = &SpaceId> {
        std::iter::once(self.id()).chain(self.carried.iter().map(|space| &space.id))
    }

    /// Consent to end the chat with the spaces that go with it, the only
    /// spaces below it its removal may take.
    pub(in super::super) fn chat_ending(&self) -> UnsavedWork {
        UnsavedWork::taking(self.carried.iter().map(|space| space.id.clone()).collect())
    }
}

/// What ending `space`, among the `spaces` there are, with the spaces
/// below it that `carrying` carries, for an export would do, refusing an
/// export that would lose work or cut its own output off with each space
/// that would.
pub(super) async fn check_export(
    ctx: &Context,
    space: Space,
    output: &Output,
    spaces: &Spaces,
    carrying: &Carrying,
) -> Result<Checked> {
    let carried = carried_spaces(spaces, &space.id, carrying);
    let name = space.name.clone();
    let ending = check_ending(ctx, space, output).await;
    let mut refusals = Vec::new();
    for below in &carried {
        if let Err(error) = check_carried(ctx, below, output).await {
            refusals.push(format!("{}: {error:#}", below.name));
        }
    }
    match ending {
        Ok(space) if refusals.is_empty() => Ok(Checked { space, carried }),
        Err(error) if refusals.is_empty() => Err(error),
        ending => {
            if let Err(error) = ending {
                refusals.insert(0, format!("{name}: {error:#}"));
            }
            bail!(
                "the spaces that would end with it cannot:\n  {}",
                refusals.join("\n  ")
            )
        }
    }
}

/// What ending `space` for an export would do, refusing an export that
/// would lose its work or cut `output` off.
async fn check_ending(ctx: &Context, space: Space, output: &Output) -> Result<Ending> {
    Ok(match space.by_kind() {
        ByKind::Workspace(workspace) => {
            Ending::Workspace(check_workspace_export(ctx, &workspace.id, output).await?)
        }
        ByKind::Chat(chat) => {
            ensure_not_running_in(&chat.id, chat.name.as_str())?;
            Ending::Chat(chat)
        }
    })
}

/// Refuse to end `space`, which goes with another, as [`check_ending`]
/// refuses a space it ends, or a workspace while another lifecycle
/// operation holds it or this terminal closes with it.
async fn check_carried(ctx: &Context, space: &Space, output: &Output) -> Result<()> {
    match space.kind() {
        SpaceKind::Workspace => {
            let check = check_workspace_export(ctx, &space.id, output).await?;
            let workspaces = super::super::super::workspaces::ensure_batch_removable;
            workspaces(ctx, &check, super::EXPORT).await?;
        }
        SpaceKind::Chat => ensure_not_running_in(&space.id, space.name.as_str())?,
    }
    Ok(())
}

/// The spaces among `spaces` that go with the space `id` as `carrying`
/// says, each after the spaces it waits for.
fn carried_spaces(spaces: &Spaces, id: &SpaceId, carrying: &Carrying) -> Vec<Space> {
    let placed: Vec<Placed<'_>> = spaces.iter().map(Placed::space).collect();
    (crate::transfer::carried_spaces(&placed, id, carrying).iter())
        .filter_map(|id| spaces.iter().find(|space| space.id == *id))
        .cloned()
        .collect()
}

/// Refuse to end the chat `id`, called `name`, when it is the one this
/// process runs in, which ending the chat would end before the export
/// finished.
fn ensure_not_running_in(id: &SpaceId, name: &str) -> Result<()> {
    ensure!(
        crate::env::own_space().as_ref() != Some(id),
        "this process runs in chat {name}, which the export ends; export it from another terminal"
    );
    Ok(())
}

/// What removing the workspace for an export would delete, refusing unsaved
/// files a checkpoint cannot keep and an output the removal would cut off.
async fn check_workspace_export(
    ctx: &Context,
    workspace: &SpaceId,
    output: &Output,
) -> Result<SpaceRemovalCheck> {
    let check =
        crate::client::removal::check(&ctx.paths, SpaceSelector::id(workspace), true).await?;
    let worktree = (check.worktree.as_ref()).context("the workspace has no worktree")?;
    // Its removal keeps no record, which the package is built from.
    ensure!(
        !worktree.workspace.disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        check.space.name
    );
    ensure!(
        !worktree.dirty || worktree.unsaved_files_archived,
        "a checkpoint cannot keep the uncommitted files (they include a repository or a changed submodule); commit or remove them before exporting"
    );
    match output.destination() {
        Some(destination) => ensure!(
            !worktree.workspace.contains(destination),
            "{} is inside the workspace the export removes; write it elsewhere",
            destination.display()
        ),
        None => ensure!(
            !crate::cli::terminal::closes_with(&ctx.paths, &check.space.id).await?,
            "this terminal closes when the export removes the workspace, ending the stream; export to a file with -o, or from another terminal"
        ),
    }
    Ok(check)
}

/// The `spaces` an export of them all exports to a file each: those that do
/// not go with another of them as `carrying` says, whose export carries
/// them, in the order given.
pub(super) fn exported_alone(spaces: &[Space], tree: &Spaces, carrying: &Carrying) -> Vec<Space> {
    let carried: Vec<Space> = (spaces.iter())
        .flat_map(|space| carried_spaces(tree, &space.id, carrying))
        .collect();
    (spaces.iter())
        .filter(|space| !carried.iter().any(|carried| carried.id == space.id))
        .cloned()
        .collect()
}

/// Ask under `heading`, unless `yes` confirms it, before ending the spaces
/// `checks` describe where that ends more than the spaces named or
/// releases holds: listing each space that goes with them, with its kind
/// and agent, and the holds the export releases, naming each hold's space
/// when there are several.
pub(super) fn confirm_ending(
    ctx: &Context,
    yes: bool,
    heading: String,
    checks: &[&Checked],
) -> Result<()> {
    let Some(action) = ending_action(heading, checks).filter(|_| !yes) else {
        return Ok(());
    };
    ensure!(ui::confirm(ctx, &action, "--yes")?, "export canceled");
    Ok(())
}

/// The question [`confirm_ending`] asks under `heading`; none when ending
/// the spaces `checks` describe ends only them and releases no holds.
fn ending_action(heading: String, checks: &[&Checked]) -> Option<String> {
    let mut carried: Vec<&Space> = Vec::new();
    for space in checks.iter().flat_map(|check| &check.carried) {
        if !carried.iter().any(|listed| listed.id == space.id) {
            carried.push(space);
        }
    }
    let own = (checks.iter()).map(|check| (check.id(), check.name().as_str(), check.own_holds()));
    let below = (carried.iter()).map(|space| (&space.id, space.name.as_str(), &space.holds[..]));
    let mut held: Vec<(&SpaceId, &str, &[SpaceHold])> = Vec::new();
    for space in own.chain(below) {
        if !space.2.is_empty() && !held.iter().any(|listed| listed.0 == space.0) {
            held.push(space);
        }
    }
    if carried.is_empty() && held.is_empty() {
        return None;
    }
    let mut action = heading;
    if !carried.is_empty() {
        action.push_str("\nEnds these spaces below here too, stopping their agents:");
        for space in &carried {
            action.push_str(&format!("\n  {} ({})", space.name, described(space)));
        }
    }
    if !held.is_empty() {
        action.push_str("\nRemoves the spaces here and releases these holds:");
    }
    for (_, name, holds) in &held {
        for hold in *holds {
            action.push_str("\n  ");
            if checks.len() > 1 || held.len() > 1 {
                action.push_str(&format!("{name}: "));
            }
            action.push_str(&hold.name);
            if let Some(reason) = &hold.reason {
                action.push_str(&format!(" ({reason})"));
            }
        }
    }
    Some(action)
}

/// The kind of `space` with its agent, as the confirmation lists it: a
/// chat's agent by name, a workspace's while it runs.
fn described(space: &Space) -> String {
    let agent = match space.clone().by_kind() {
        ByKind::Chat(chat) => chat.agent.as_ref().map(ToString::to_string),
        ByKind::Workspace(_) => space.running.then(|| "agent running".to_owned()),
    };
    match agent {
        Some(agent) => format!("{}, {agent}", space.kind()),
        None => space.kind().to_string(),
    }
}

#[cfg(test)]
mod tests;
