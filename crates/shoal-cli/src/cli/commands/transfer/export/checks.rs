//! What ending a space for its export would do, the refusals of an export
//! that would lose work or cut its own output off, and the confirmation
//! ending the spaces needs.
use anyhow::{Result, ensure};

use super::Output;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{context::Context, ui},
    model::{Chat, PresentSpace, SpaceHold, SpaceKind, SpaceRef, Spaces},
    removal::{RemovalCheck, UnsavedWork},
};

/// A space that passed its export check, with the spaces its removal takes
/// first and the chats among them that go with it, which its export ends
/// too.
pub(in super::super) struct Checked {
    pub(in super::super) space: Ending,
    taken: Vec<SpaceRef>,
    carried: Vec<Chat>,
}

/// What ending a space for its export does: removing a workspace, with
/// what its removal would delete, or a chat whose agent reported a session.
#[allow(
    clippy::large_enum_variant,
    reason = "one per exported space, moved into its planned export"
)]
pub(in super::super) enum Ending {
    Workspace(RemovalCheck),
    Chat(Chat),
}

impl Checked {
    pub(super) fn name(&self) -> &crate::client::space_names::SpaceName {
        match &self.space {
            Ending::Workspace(check) => &check.workspace.name,
            Ending::Chat(chat) => &chat.name,
        }
    }

    pub(in super::super) fn id(&self) -> &crate::client::space_ids::SpaceId {
        match &self.space {
            Ending::Workspace(check) => &check.workspace.id,
            Ending::Chat(chat) => &chat.id,
        }
    }

    /// The space with its kind, as a confirmation names it.
    pub(super) fn label(&self) -> String {
        match &self.space {
            Ending::Workspace(check) => format!("workspace {}", check.workspace.name),
            Ending::Chat(chat) => format!("chat {}", chat.name),
        }
    }

    /// The space and the chats that go with it, each with its holds, which
    /// ending them here releases.
    fn holds(&self) -> impl Iterator<Item = (&SpaceId, &SpaceName, &[SpaceHold])> {
        let own = match &self.space {
            Ending::Workspace(check) => (
                &check.workspace.id,
                &check.workspace.name,
                &check.workspace.holds,
            ),
            Ending::Chat(chat) => (&chat.id, &chat.name, &chat.holds),
        };
        let carried = (self.carried.iter()).map(|chat| (&chat.id, &chat.name, &chat.holds));
        std::iter::once(own)
            .chain(carried)
            .map(|(id, name, holds)| (id, name, holds.as_slice()))
    }

    /// The space and the chats that go with it, whose agents a move stops.
    pub(in super::super) fn spaces(&self) -> impl Iterator<Item = &SpaceId> {
        std::iter::once(self.id()).chain(self.carried.iter().map(|chat| &chat.id))
    }

    /// Consent to end the chat with the chats that go with it, the only
    /// spaces below it its removal may take.
    pub(in super::super) fn chat_ending(&self) -> UnsavedWork {
        UnsavedWork::taking(self.carried.iter().map(|chat| chat.id.clone()).collect())
    }

    /// Refuse to export the space without the workspaces below it, which
    /// removing it here would take unexported, unless they are among the
    /// `exported` too; the chats below those go with them.
    pub(super) fn ensure_children_exported(&self, exported: &[&SpaceId]) -> Result<()> {
        let missing: Vec<&str> = (self.taken.iter())
            .filter(|space| space.kind == SpaceKind::Workspace && !exported.contains(&&space.id))
            .map(|space| space.name.as_str())
            .collect();
        ensure!(
            missing.is_empty(),
            "workspaces are below it ({}); export them with it, as shoal move export --all does, or remove them first",
            missing.join(", ")
        );
        Ok(())
    }
}

/// What ending `space`, among the `spaces` there are, for an export would
/// do, refusing an export that would lose work or cut its own output off.
pub(super) async fn check_export(
    ctx: &Context,
    space: PresentSpace,
    output: &Output,
    spaces: &Spaces,
) -> Result<Checked> {
    let carried = carried_chats(spaces, space.id());
    for chat in &carried {
        ensure_not_running_in(chat)?;
    }
    let (space, taken) = match space {
        PresentSpace::Workspace(workspace) => {
            let check = check_workspace_export(ctx, &workspace.id, output).await?;
            let taken = check.children.clone();
            (Ending::Workspace(check), taken)
        }
        PresentSpace::Chat(chat) => {
            let taken = spaces.taken_with(&chat.id);
            (Ending::Chat(check_chat_export(chat)?), taken)
        }
    };
    Ok(Checked {
        space,
        taken,
        carried,
    })
}

/// The chats among `spaces` that go with the space `id`, each after its
/// parent among them.
fn carried_chats(spaces: &Spaces, id: &SpaceId) -> Vec<Chat> {
    (crate::transfer::carried_chats(&spaces.chats, id).iter())
        .filter_map(|id| spaces.chats.iter().find(|chat| chat.id == *id))
        .cloned()
        .collect()
}

/// Refuse to export a chat whose agent reported no session, which would not
/// continue elsewhere, or as [`ensure_not_running_in`] refuses.
fn check_chat_export(chat: Chat) -> Result<Chat> {
    ensure!(
        chat.session.is_some(),
        "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
        chat.name
    );
    ensure_not_running_in(&chat)?;
    Ok(chat)
}

/// Refuse to end `chat`, the one this process runs in, which ending the
/// chat would end before the export finished.
fn ensure_not_running_in(chat: &Chat) -> Result<()> {
    ensure!(
        crate::env::chat().as_ref() != Some(&chat.id),
        "this process runs in chat {}, which the export ends; export it from another terminal",
        chat.name
    );
    Ok(())
}

/// What removing the workspace for an export would delete, refusing unsaved
/// files a checkpoint cannot keep and an output the removal would cut off.
async fn check_workspace_export(
    ctx: &Context,
    workspace: &SpaceId,
    output: &Output,
) -> Result<RemovalCheck> {
    let check =
        crate::client::removal::check(&ctx.paths, SpaceSelector::id(workspace), true).await?;
    // Its removal keeps no record, which the package is built from.
    ensure!(
        !check.workspace.disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        check.workspace.name
    );
    ensure!(
        !check.dirty || check.unsaved_files_archived,
        "a checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule); commit or remove them before exporting"
    );
    match output.destination() {
        Some(destination) => ensure!(
            !check.workspace.contains(destination),
            "{} is inside the workspace the export removes; write it elsewhere",
            destination.display()
        ),
        None => ensure!(
            !crate::cli::terminal::closes_with(&ctx.paths, &check.workspace.id).await?,
            "this terminal closes when the export removes the workspace, ending the stream; export to a file with -o, or from another terminal"
        ),
    }
    Ok(check)
}

/// The `spaces` an export of them all ends one by one: those that do not go
/// with another of them, which its export ends, each after the spaces below
/// it that its removal would otherwise take.
pub(super) fn exported_alone(spaces: &[PresentSpace], tree: &Spaces) -> Vec<PresentSpace> {
    let carried: Vec<SpaceId> = (spaces.iter())
        .flat_map(|space| crate::transfer::carried_chats(&tree.chats, space.id()))
        .collect();
    let mut alone: Vec<PresentSpace> = (spaces.iter())
        .filter(|space| !carried.contains(space.id()))
        .cloned()
        .collect();
    alone.sort_by_key(|space| tree.taken_with(space.id()).len());
    alone
}

/// Ask under `heading`, unless `yes` confirms it, before ending the spaces
/// `checks` describe where that needs the user: a chat, which the user
/// talks with, with the chats that go with them, or a space with holds that
/// external sessions keep, which the export releases, naming each hold's
/// space when there are several.
pub(super) fn confirm_ending(
    ctx: &Context,
    yes: bool,
    heading: String,
    checks: &[&Checked],
) -> Result<()> {
    let mut chats: Vec<&Chat> = Vec::new();
    for check in checks {
        let own = match &check.space {
            Ending::Chat(chat) => Some(chat),
            Ending::Workspace(_) => None,
        };
        for chat in own.into_iter().chain(&check.carried) {
            if !chats.iter().any(|listed| listed.id == chat.id) {
                chats.push(chat);
            }
        }
    }
    let mut held: Vec<(&SpaceId, &SpaceName, &[SpaceHold])> = Vec::new();
    for space in checks.iter().flat_map(|check| check.holds()) {
        if !space.2.is_empty() && !held.iter().any(|listed| listed.0 == space.0) {
            held.push(space);
        }
    }
    if yes || (chats.is_empty() && held.is_empty()) {
        return Ok(());
    }
    let mut action = heading;
    if !chats.is_empty() {
        action.push_str("\nEnds these chats here, stopping their agents:");
        for chat in &chats {
            action.push_str(&format!("\n  {} ({})", chat.name, chat.agent));
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
    ensure!(ui::confirm(ctx, &action, "--yes")?, "export canceled");
    Ok(())
}

#[cfg(test)]
mod tests;
