//! What ending a space for its move would do, and the refusals of a move
//! that would lose work or write its package into what it ends.
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_protocol::model::ByKind;

use crate::{
    client::connection::CommandPaths,
    model::{Chat, Space, SpaceHold, SpaceKind, Spaces},
    state::WorkspaceState,
};
use shoal_core::transfer::{Carrying, Placed};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::removal::{SpaceRemovalCheck, UnsavedWork};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// A space that passed its move's check, with the spaces below it that go
/// with it, which its end takes too.
pub struct Checked {
    pub space: Ending,
    pub carried: Vec<Space>,
}

/// What ending a space for its move does: removing a workspace, with what
/// its removal would delete, or a chat.
#[allow(
    clippy::large_enum_variant,
    reason = "one per moved space, kept with its planned move"
)]
pub enum Ending {
    Workspace(SpaceRemovalCheck),
    Chat(Chat),
}

impl Checked {
    pub fn name(&self) -> &SpaceName {
        match &self.space {
            Ending::Workspace(check) => &check.space.name,
            Ending::Chat(chat) => &chat.name,
        }
    }

    pub fn id(&self) -> &SpaceId {
        match &self.space {
            Ending::Workspace(check) => &check.space.id,
            Ending::Chat(chat) => &chat.id,
        }
    }

    /// The space's holds, which ending it here releases.
    pub fn own_holds(&self) -> &[SpaceHold] {
        match &self.space {
            Ending::Workspace(check) => &check.space.holds,
            Ending::Chat(chat) => &chat.holds,
        }
    }

    /// Consent to delete what the check shows and the spaces that go with
    /// it, the only spaces below it its end may take.
    pub fn unsaved(&self) -> UnsavedWork {
        match &self.space {
            Ending::Workspace(check) => UnsavedWork::confirming(check),
            Ending::Chat(_) => {
                UnsavedWork::taking(self.carried.iter().map(|space| space.id.clone()).collect())
            }
        }
    }
}

/// What ending `space`, among the `spaces` there are, with the spaces
/// below it that `carrying` carries, for a move to the package `file`
/// would do, refusing a move that would lose work or end the space its
/// package goes into, with each space that would.
pub async fn check_export(
    paths: &CommandPaths,
    space: Space,
    file: Option<&Path>,
    spaces: &Spaces,
    carrying: &Carrying,
) -> Result<Checked> {
    let carried = carried_spaces(spaces, &space.id, carrying);
    let name = space.name.clone();
    let ending = check_ending(paths, space, file).await;
    let mut refusals = Vec::new();
    for below in &carried {
        if let Err(error) = check_carried(paths, below, file).await {
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

/// What ending `space` for a move would do, refusing a move that would lose
/// its work or end the space `file` goes into.
async fn check_ending(paths: &CommandPaths, space: Space, file: Option<&Path>) -> Result<Ending> {
    Ok(match space.by_kind() {
        ByKind::Workspace(workspace) => {
            Ending::Workspace(check_workspace_export(paths, &workspace.id, file).await?)
        }
        ByKind::Chat(chat) => Ending::Chat(chat),
    })
}

/// Refuse to end `space`, which goes with another, as [`check_ending`]
/// refuses a space it ends, or a workspace while another lifecycle
/// operation holds it.
async fn check_carried(paths: &CommandPaths, space: &Space, file: Option<&Path>) -> Result<()> {
    if space.kind() == SpaceKind::Workspace {
        let check = check_workspace_export(paths, &space.id, file).await?;
        ensure_settled(&check)?;
    }
    Ok(())
}

/// The spaces among `spaces` that go with the space `id` as `carrying`
/// says, each after the spaces it waits for.
pub fn carried_spaces(spaces: &Spaces, id: &SpaceId, carrying: &Carrying) -> Vec<Space> {
    let placed: Vec<Placed<'_>> = spaces.iter().map(Placed::space).collect();
    (shoal_core::transfer::carried_spaces(&placed, id, carrying).iter())
        .filter_map(|id| spaces.iter().find(|space| space.id == *id))
        .cloned()
        .collect()
}

/// What removing the workspace for a move would delete, refusing unsaved
/// files a checkpoint cannot keep and a package `file` inside it.
async fn check_workspace_export(
    paths: &CommandPaths,
    workspace: &SpaceId,
    file: Option<&Path>,
) -> Result<SpaceRemovalCheck> {
    let check = crate::client::removal::check(paths, SpaceSelector::id(workspace), true).await?;
    let worktree = (check.worktree.as_ref()).context("the workspace has no worktree")?;
    // Its removal keeps no record, which the package is built from.
    ensure!(
        !worktree.workspace.disposable,
        "workspace {} is disposable, so its removal keeps nothing to move",
        check.space.name
    );
    ensure!(
        !worktree.dirty || worktree.unsaved_files_archived,
        "a checkpoint cannot keep the uncommitted files (they include a repository or a changed submodule); commit or remove them before moving it"
    );
    if let Some(file) = file {
        ensure!(
            !worktree.workspace.contains(file),
            "{} is inside the workspace the move removes; write it elsewhere",
            file.display()
        );
    }
    Ok(check)
}

/// Refuse to end a workspace while another lifecycle operation holds it.
pub fn ensure_settled(check: &SpaceRemovalCheck) -> Result<()> {
    if let Some(worktree) = &check.worktree {
        let state = worktree.workspace.state;
        ensure!(
            match state {
                WorkspaceState::Ready | WorkspaceState::Failed => true,
                WorkspaceState::Preparing => worktree.running_commands == 0,
                _ => false,
            },
            "workspace is {state}; wait for that operation to finish"
        );
    }
    Ok(())
}

/// The `spaces` a move of them all packs into a package each: those that
/// do not go with another of them as `carrying` says, whose package carries
/// them, in the order given.
pub fn exported_alone(spaces: &[Space], tree: &Spaces, carrying: &Carrying) -> Vec<Space> {
    let carried: Vec<Space> = (spaces.iter())
        .flat_map(|space| carried_spaces(tree, &space.id, carrying))
        .collect();
    (spaces.iter())
        .filter(|space| !carried.iter().any(|carried| carried.id == space.id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
