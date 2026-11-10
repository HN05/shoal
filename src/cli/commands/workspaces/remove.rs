//! Removing spaces after checking what each loses.
use anyhow::{Result, ensure};

use super::removal::{ensure_batch_removable, leaving};
use crate::client::output::workspaces::{Removals, Removed};
use crate::client::space_names::{self, SpaceName, SpaceSelector};
use crate::{
    cli::{
        commands::{chat::removal as chats, queued_agents},
        context::Context,
        output::Style,
        ui,
    },
    model::{Chat, PresentSpace, SpaceKind, SpaceRef, Spaces},
    removal::{RemovalCheck, RemovalResult, UnsavedWork},
};

pub(in crate::cli::commands) async fn remove(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    yes: bool,
) -> Result<i32> {
    // A number always names a space, never a queued launch.
    if let [SpaceSelector::IdOrName(selector)] = names.as_slice()
        && let Some(code) = queued_agents::remove(ctx, selector).await?
    {
        return Ok(code);
    }
    let selected = ui::select_spaces(ctx, names, true).await?;
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let (chats, workspaces) = named_chats(&spaces, selected);
    let checks = check_removals(ctx, workspaces, yes).await?;
    if !yes {
        confirm_removals(ctx, &chats, &checks, &spaces)?;
    }
    if let [chat] = chats.as_slice()
        && checks.is_empty()
    {
        return remove_one_chat(ctx, chat, &spaces).await;
    }
    match <[RemovalCheck; 1]>::try_from(checks) {
        Ok([check]) if chats.is_empty() => remove_one(ctx, check).await,
        Ok(check) => remove_each(ctx, &chats, check.into(), &spaces).await,
        Err(checks) => remove_each(ctx, &chats, checks, &spaces).await,
    }
}

/// The chats among the spaces `names` select, each once, and the other
/// names, which name workspaces or nothing.
fn named_chats(spaces: &Spaces, names: Vec<SpaceSelector>) -> (Vec<Chat>, Vec<SpaceSelector>) {
    let mut chats: Vec<Chat> = Vec::new();
    let mut others = Vec::new();
    for name in names {
        match spaces.find(&name) {
            Some(PresentSpace::Chat(chat)) => {
                if !chats.iter().any(|named| named.id == chat.id) {
                    chats.push(chat);
                }
            }
            _ => others.push(name),
        }
    }
    (chats, others)
}

/// Check every workspace before removing any, so one that cannot be removed
/// leaves the others in place.
async fn check_removals(
    ctx: &Context,
    workspaces: Vec<SpaceSelector>,
    yes: bool,
) -> Result<Vec<RemovalCheck>> {
    let mut checks = Vec::new();
    let mut refusals = Vec::new();
    let several = workspaces.len() > 1;
    for workspace in workspaces {
        let include_changes = ctx.interactive() && !yes;
        let checked =
            crate::client::removal::check(&ctx.paths, workspace.clone(), include_changes).await;
        if !several {
            checks.push(checked?);
            continue;
        }
        match checked {
            Ok(check) => match ensure_batch_removable(ctx, &check, ("removal", "remove")).await {
                Ok(()) => checks.push(check),
                Err(error) => refusals.push(format!("{}: {error:#}", check.workspace.name)),
            },
            Err(error) => refusals.push(format!("{workspace}: {error:#}")),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot remove these workspaces, so none was removed:\n  {}",
        refusals.join("\n  ")
    );
    Ok(checks)
}

async fn remove_one_chat(ctx: &Context, chat: &Chat, spaces: &Spaces) -> Result<i32> {
    let removed = chats::remove(ctx, chat, spaces).await?;
    let message = chats::message("Chat removed", &removed);
    ctx.emit_styled(Style::Success, &message, removed)?;
    Ok(0)
}

async fn remove_one(ctx: &Context, check: RemovalCheck) -> Result<i32> {
    let result = remove_checked(ctx, &check).await?;
    let message = removal_message("Workspace removed", &result);
    ctx.emit_styled(Style::Success, &message, removed(result, &check, None))?;
    Ok(0)
}

/// Remove each of the `chats`, then each checked workspace, going on after
/// one fails. One that a removal before it took with its own is not removed
/// again.
async fn remove_each(
    ctx: &Context,
    chats: &[Chat],
    checks: Vec<RemovalCheck>,
    spaces: &Spaces,
) -> Result<i32> {
    let mut removed_chats = ui::Report::new("remove");
    let mut taken: Vec<SpaceName> = Vec::new();
    for chat in chats {
        if taken.contains(&chat.name) {
            continue;
        }
        match chats::remove(ctx, chat, spaces).await {
            Ok(removed) => {
                let message = chats::message(&format!("Removed {}", chat.name), &removed);
                taken.extend(removed.children.iter().cloned());
                removed_chats.done(ctx, &message, removed);
            }
            Err(error) => removed_chats.failed(ctx, &chat.name, &error),
        }
    }
    let (chats, failed) = removed_chats.into_parts();
    let mut report = ui::Report::new("remove");
    for check in checks {
        let name = &check.workspace.name;
        if taken.contains(name) {
            continue;
        }
        match remove_checked(ctx, &check).await {
            Ok(result) => {
                let message = removal_message(&format!("Removed {name}"), &result);
                taken.extend(result.children.iter().cloned());
                report.done(ctx, &message, removed(result, &check, Some(name)));
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let chats_failed = !failed.is_empty();
    let code = report.finish(ctx, "", |removed, more| Removals {
        removed,
        chats,
        failed: failed.into_iter().chain(more).collect(),
    })?;
    // A chat that was not removed fails the command as a workspace does.
    Ok(if chats_failed { 1 } else { code })
}

/// Remove the workspace `check` inspected, deleting only the unsaved work it
/// saw: more appearing meanwhile makes the daemon refuse.
async fn remove_checked(ctx: &Context, check: &RemovalCheck) -> Result<RemovalResult> {
    let unsaved = UnsavedWork::confirming(check);
    let result = leaving(
        ctx,
        &check.workspace,
        crate::client::removal::remove(&ctx.paths, SpaceSelector::id(&check.workspace.id), unsaved),
    )
    .await?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    Ok(result)
}

/// What JSON reports for a removal: its result and the holds of the
/// workspace, which a removal of several names.
fn removed(removal: RemovalResult, check: &RemovalCheck, name: Option<&SpaceName>) -> Removed {
    Removed {
        workspace: name.cloned(),
        removal,
        holds: check.workspace.holds.clone(),
    }
}

/// Ask once before removing `chats` that take workspaces or stop an agent,
/// or workspaces that would lose unsaved work or holds, naming what each
/// loses.
fn confirm_removals(
    ctx: &Context,
    chats: &[Chat],
    checks: &[RemovalCheck],
    spaces: &Spaces,
) -> Result<()> {
    let chat_losses = chats
        .iter()
        .map(|chat| (chat.name.to_string(), chats::losses(chat, spaces)));
    let losses: Vec<(String, Vec<String>)> = chat_losses
        .chain(
            checks
                .iter()
                .map(|check| (check.workspace.name.to_string(), removal_losses(ctx, check))),
        )
        .collect();
    if losses.iter().all(|(_, lines)| lines.is_empty()) {
        return Ok(());
    }
    let kind = match (chats, checks) {
        ([_], []) => "chat",
        ([], [_]) => "workspace",
        (_, []) => "chats",
        ([], _) => "workspaces",
        _ => "spaces",
    };
    let action = match losses.as_slice() {
        [(name, _)] => format!("Remove {kind}: {name}"),
        _ => format!(
            "Remove {} {kind}: {}",
            losses.len(),
            losses
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    ensure!(
        ui::confirm_losses(ctx, &action, &losses, "--yes")?,
        "removal canceled"
    );
    Ok(())
}

/// What removing the workspace `check` inspected loses: the workspaces
/// below it and the chats in it and in them, its unsaved work, with the
/// changed files on a terminal, and its holds. Nothing in a disposable
/// workspace counts.
fn removal_losses(ctx: &Context, check: &RemovalCheck) -> Vec<String> {
    let mut lines = Vec::new();
    let unsaved = check.unsaved_work();
    let holds = match check.workspace.disposable {
        true => &[][..],
        false => &check.workspace.holds[..],
    };
    let (chats, children): (Vec<_>, Vec<_>) = check
        .children
        .iter()
        .filter(|child| !check.disposable_children.contains(&child.id))
        .partition(|child| child.kind == SpaceKind::Chat);
    if unsaved.is_empty() && holds.is_empty() && children.is_empty() && chats.is_empty() {
        return lines;
    }
    let names = |spaces: &[&SpaceRef]| {
        let names: Vec<_> = spaces.iter().map(|space| space.name.as_str()).collect();
        names.join(", ")
    };
    if !children.is_empty() {
        lines.push(format!(
            "Removes the workspaces split from it first, whatever their work or holds; shoal restore brings each back: {}",
            names(&children)
        ));
    }
    if !chats.is_empty() {
        lines.push(format!("Removes the chats in it first: {}", names(&chats)));
    }
    for work in &unsaved {
        lines.push(format!("Deletes: {work}"));
    }
    if !unsaved.is_empty() {
        lines.push(if !check.dirty || check.unsaved_files_archived {
            "shoal restore brings them back until the checkpoint expires.".into()
        } else {
            "The checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule), so they cannot be restored.".into()
        });
    }
    if !holds.is_empty() {
        lines.push("Holds:".into());
        for hold in holds {
            let reason = crate::cli::context::optional(hold.reason.as_deref(), |reason| {
                format!(" ({reason})")
            });
            lines.push(format!("  {}{reason}", hold.name));
        }
    }
    if ctx.interactive() && (!check.changed_files.is_empty() || check.changed_files_omitted > 0) {
        lines.push("Uncommitted changes and untracked files (Git status):".into());
        for file in &check.changed_files {
            lines.push(format!("  {file}"));
        }
        if check.changed_files_omitted > 0 {
            lines.push(format!(
                "  ... {} more entries omitted; run git status in the workspace for the full list",
                check.changed_files_omitted
            ));
        }
    }
    lines
}

/// `removed`, such as `Workspace removed`, with the Git branch the removal
/// kept and the workspaces below it it took.
fn removal_message(removed: &str, result: &RemovalResult) -> String {
    let mut message = match &result.branch {
        Some(branch) if !result.branch_outcome.is_deleted() => {
            format!("{removed}; kept Git branch {branch}")
        }
        _ => removed.into(),
    };
    if !result.children.is_empty() {
        message.push_str(&format!(
            "; also removed {}",
            space_names::join(&result.children, ", ")
        ));
    }
    message
}
