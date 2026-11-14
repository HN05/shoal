//! What ending a space for its export would do, the refusals of an export
//! that would lose work or cut its own output off, and the confirmation
//! ending the spaces needs.
use anyhow::{Result, bail, ensure};

use super::Output;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    model::{Chat, PresentSpace},
    removal::RemovalCheck,
};

/// A space that passed its export check: a workspace with what its removal
/// would delete, or a running chat whose agent reported a session.
#[allow(
    clippy::large_enum_variant,
    reason = "one per exported space, moved into its planned export"
)]
pub(in super::super) enum Checked {
    Workspace(RemovalCheck),
    Chat(Chat),
}

impl Checked {
    pub(super) fn name(&self) -> &crate::client::space_names::SpaceName {
        match self {
            Self::Workspace(check) => &check.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    pub(super) fn id(&self) -> &crate::client::space_ids::SpaceId {
        match self {
            Self::Workspace(check) => &check.workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    /// The space with its kind, as a confirmation names it.
    pub(super) fn label(&self) -> String {
        match self {
            Self::Workspace(check) => format!("workspace {}", check.workspace.name),
            Self::Chat(chat) => format!("chat {}", chat.name),
        }
    }

    /// Refuse to export a workspace without the workspaces below it, which
    /// removing it here would take unexported, unless they are among the
    /// `exported` too.
    pub(super) fn ensure_children_exported(&self, exported: &[&SpaceId]) -> Result<()> {
        match self {
            Self::Workspace(check) => ensure_children_exported(check, exported),
            Self::Chat(_) => Ok(()),
        }
    }
}

/// What ending `space` for an export would do, refusing an export that
/// would lose work or cut its own output off.
pub(super) async fn check_export(
    ctx: &Context,
    space: PresentSpace,
    output: &Output,
) -> Result<Checked> {
    match space {
        PresentSpace::Workspace(workspace) => check_workspace_export(ctx, &workspace.id, output)
            .await
            .map(Checked::Workspace),
        PresentSpace::Chat(chat) => check_chat_export(chat).map(Checked::Chat),
    }
}

/// Refuse to export a chat whose agent reported no session, which would not
/// continue elsewhere, or the one this process runs in, which ending the
/// chat would end before the export finished.
fn check_chat_export(chat: Chat) -> Result<Chat> {
    if let Some(workspace) = &chat.workspace {
        bail!(
            "chat {} runs in workspace {}, and shoal move does not move a chat in a workspace",
            chat.name,
            workspace.name
        );
    }
    ensure!(
        chat.session.is_some(),
        "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
        chat.name
    );
    ensure!(
        crate::env::chat().as_ref() != Some(&chat.id),
        "this process runs in chat {}, which the export ends; export it from another terminal",
        chat.name
    );
    Ok(chat)
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

/// Refuse to export the workspace `check` describes without the workspaces
/// below it, which removing it here would take unexported, unless they are
/// among the `exported` too.
fn ensure_children_exported(check: &RemovalCheck, exported: &[&SpaceId]) -> Result<()> {
    let (chats, workspaces): (Vec<_>, Vec<_>) =
        (check.children.iter()).partition(|child| child.kind == crate::model::SpaceKind::Chat);
    let chats: Vec<&str> = chats.iter().map(|chat| chat.name.as_str()).collect();
    ensure!(
        chats.is_empty(),
        "chats run in it or below it ({}), which shoal move does not move; remove them first",
        chats.join(", ")
    );
    let missing: Vec<&str> = workspaces
        .iter()
        .filter(|child| !exported.contains(&&child.id))
        .map(|child| child.name.as_str())
        .collect();
    ensure!(
        missing.is_empty(),
        "workspaces were split from it ({}); export them with it, as shoal move export --all does, or remove them first",
        missing.join(", ")
    );
    Ok(())
}

/// Ask under `heading`, unless `yes` confirms it, before ending the spaces
/// `checks` describe where that needs the user: a chat, which the user
/// talks with, or a workspace with holds that external sessions keep, which
/// the export releases, naming each hold's workspace when there are several.
pub(super) fn confirm_ending(
    ctx: &Context,
    yes: bool,
    heading: String,
    checks: &[&Checked],
) -> Result<()> {
    let chats: Vec<&Chat> = checks
        .iter()
        .filter_map(|check| match check {
            Checked::Chat(chat) => Some(chat),
            Checked::Workspace(_) => None,
        })
        .collect();
    let held: Vec<&RemovalCheck> = checks
        .iter()
        .filter_map(|check| match check {
            Checked::Workspace(check) if !check.workspace.holds.is_empty() => Some(check),
            _ => None,
        })
        .collect();
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
        action.push_str("\nRemoves the workspaces here and releases these holds:");
    }
    for check in &held {
        for hold in &check.workspace.holds {
            action.push_str("\n  ");
            if checks.len() > 1 {
                action.push_str(&format!("{}: ", check.workspace.name));
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
