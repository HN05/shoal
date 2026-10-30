//! `shoal files` lists the files attached to a workspace or chat, or on a
//! terminal opens a picker over them; `add` attaches more without a message
//! and `rm` deletes them. A chat's agent acts on its chat's files unless it
//! names another owner.
use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::WorkspaceScope,
        client,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::workspace_files::{AttachedFile, FileSource},
    protocol::FileMethod,
};

mod menu;

/// How `shoal files` shows a workspace's or chat's files.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Listing {
    /// A picker on a terminal, otherwise a list.
    Menu,
    /// A list.
    Print,
    /// Only where Shoal keeps them.
    Paths,
}

/// Show the files of the workspace or chat `owner` names as `listing` asks.
pub(super) async fn list(ctx: &Context, owner: Option<String>, listing: Listing) -> Result<i32> {
    let owner = select_owner(ctx, owner).await?;
    let files = client::pages::<AttachedFile>(
        &ctx.paths,
        FileMethod::ListFiles {
            owner: owner.clone(),
        },
    )
    .await?;
    if listing == Listing::Menu && ctx.interactive_output() {
        return menu::choose(ctx, owner, &files).await;
    }
    let paths = listing == Listing::Paths;
    ctx.show(&files, |files| {
        if paths {
            for file in files {
                outln!("{}", file.path.display());
            }
            return;
        }
        let now = crate::time::unix_seconds() as i64;
        let rows = files.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
        if files.is_empty() {
            outln!("No files");
        }
    })?;
    Ok(0)
}

/// Attach copies of `files` to the workspace or chat `owner` names,
/// checking each before a workspace is picked.
pub(super) async fn add(ctx: &Context, owner: Option<String>, files: Vec<String>) -> Result<i32> {
    let files = super::messages::attachments(&files)?;
    let owner = select_owner(ctx, owner).await?;
    let method = FileMethod::AddFiles {
        owner,
        files,
        from_chat: crate::env::acting_chat(),
    };
    let added = client::pages::<AttachedFile>(&ctx.paths, method).await?;
    ctx.show(&added, |added| {
        let now = crate::time::unix_seconds() as i64;
        let rows = added.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
    })?;
    Ok(0)
}

/// Delete the files of the workspace or chat `scope` selects, only those
/// with one of `names` and the records `ids` when there are any, or of every
/// workspace without a running agent or command and every ended chat. Asks
/// first for all of them, or a workspace where an agent or command runs, or
/// another running chat, unless `yes`.
pub(super) async fn remove(
    ctx: &Context,
    scope: WorkspaceScope,
    names: Vec<String>,
    ids: Vec<i64>,
    yes: bool,
) -> Result<i32> {
    // Names and records belong to one owner, so they offer no choice of
    // every owner.
    let owner = match own_chat(&scope.workspace) {
        Some(chat) if !scope.all => Some(chat),
        _ if names.is_empty() && ids.is_empty() => ui::select_workspace_filter(ctx, scope).await?,
        _ => Some(select_owner(ctx, scope.workspace).await?),
    };
    if let Some(owner) = &owner {
        check_selected(ctx, owner, &names, &ids).await?;
    }
    if !yes && let Some(action) = confirmation(ctx, owner.as_deref()).await? {
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let deleted =
        client::pages::<AttachedFile>(&ctx.paths, FileMethod::RemoveFiles { owner, names, ids })
            .await?;
    let text = match deleted.len() {
        0 => "No files deleted".to_owned(),
        1 => "Deleted 1 file".to_owned(),
        n => format!("Deleted {n} files"),
    };
    ctx.emit(&text, &deleted)?;
    Ok(0)
}

/// The workspace or chat whose files a command acts on: the one `owner`
/// names, else the chat this runs under, else the current workspace or the
/// one picked.
async fn select_owner(ctx: &Context, owner: Option<String>) -> Result<String> {
    match own_chat(&owner) {
        Some(chat) => Ok(chat),
        None => ui::select_workspace(ctx, owner, Fallback::CurrentDirectory).await,
    }
}

/// The chat this acts for, when `owner` names none.
fn own_chat(owner: &Option<String>) -> Option<String> {
    owner.is_none().then(crate::env::acting_chat).flatten()
}

/// Refuse names and records the owner's files do not have before asking
/// anything; the daemon checks again as it deletes.
async fn check_selected(ctx: &Context, owner: &str, names: &[String], ids: &[i64]) -> Result<()> {
    if names.is_empty() && ids.is_empty() {
        return Ok(());
    }
    let method = FileMethod::ListFiles {
        owner: owner.to_owned(),
    };
    let files = client::pages::<AttachedFile>(&ctx.paths, method).await?;
    let missing = crate::daemon::workspace_files::missing_selection(&files, names, ids);
    ensure!(
        missing.is_empty(),
        "{owner} has no attached file {}",
        missing.join(", ")
    );
    Ok(())
}

/// What deleting the files of `owner`, or of every workspace and ended
/// chat, asks the user to confirm, if anything.
async fn confirmation(ctx: &Context, owner: Option<&str>) -> Result<Option<String>> {
    let Some(owner) = owner else {
        return Ok(Some(
            "Delete the attached files of every workspace without a running agent or command and of every ended chat"
                .into(),
        ));
    };
    let workspaces = client::workspaces(&ctx.paths).await?;
    if let Some(workspace) = workspaces
        .iter()
        .find(|listed| listed.id == owner || listed.name == owner)
    {
        return Ok(workspace.running.then(|| {
            format!(
                "An agent or command runs in {}. Delete its attached files",
                workspace.name
            )
        }));
    }
    // A chat's own agent deletes its files without asking.
    let chats = client::chats(&ctx.paths).await?;
    Ok(chats
        .iter()
        .find(|chat| chat.id == owner || chat.name == owner)
        .filter(|chat| crate::env::acting_chat().as_deref() != Some(chat.id.as_str()))
        .map(|chat| format!("Chat {} runs. Delete its attached files", chat.name)))
}

/// The file's name, title, size, how it came, when, read at `now`, when a
/// removed workspace's file is deleted, and Shoal's copy.
fn cells(file: &AttachedFile, now: i64) -> [ui::Cell; 7] {
    let expiry = file
        .expires_at
        .map(|expires_at| format!("until {}", crate::time::local_minutes(expires_at)));
    [
        (file.name.clone(), Some(Style::Heading)),
        (file.title.clone().unwrap_or_default(), None),
        (ui::size_label(file.bytes), None),
        (source_label(file.source).into(), None),
        (
            crate::time::local_ago(file.created_at, now),
            Some(Style::Muted),
        ),
        (expiry.unwrap_or_default(), Some(Style::Warning)),
        (file.path.display().to_string(), Some(Style::Muted)),
    ]
}

fn source_label(source: FileSource) -> &'static str {
    match source {
        FileSource::Message => "message",
        FileSource::Added => "added",
        FileSource::Result => "result",
    }
}
