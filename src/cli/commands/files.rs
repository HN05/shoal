//! `shoal files` lists the files attached to a workspace or chat, or on a
//! terminal opens a picker over them; `add` attaches more without a message
//! and `rm` deletes them. Under a chat's agent they act on its chat.
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

/// How `shoal files` shows a workspace's files.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Listing {
    /// A picker on a terminal, otherwise a list.
    Menu,
    /// A list.
    Print,
    /// Only where Shoal keeps them.
    Paths,
}

/// Show the files of the workspace or chat as `listing` asks.
pub(super) async fn list(
    ctx: &Context,
    workspace: Option<String>,
    listing: Listing,
) -> Result<i32> {
    let workspace = select_owner(ctx, workspace).await?;
    let files = client::pages::<AttachedFile>(
        &ctx.paths,
        FileMethod::ListFiles {
            workspace: workspace.clone(),
        },
    )
    .await?;
    if listing == Listing::Menu && ctx.interactive_output() {
        return menu::choose(ctx, workspace, &files).await;
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

/// Attach copies of `files` to the workspace or chat, checking each before
/// a workspace is picked.
pub(super) async fn add(
    ctx: &Context,
    workspace: Option<String>,
    files: Vec<String>,
) -> Result<i32> {
    let files = super::messages::attachments(&files)?;
    let workspace = select_owner(ctx, workspace).await?;
    let added =
        client::pages::<AttachedFile>(&ctx.paths, FileMethod::AddFiles { workspace, files })
            .await?;
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
/// workspace without a running agent or command. Asks first for every
/// workspace, or one where an agent or command runs, unless `yes`.
pub(super) async fn remove(
    ctx: &Context,
    scope: WorkspaceScope,
    names: Vec<String>,
    ids: Vec<i64>,
    yes: bool,
) -> Result<i32> {
    // Names and records belong to one workspace or chat, so they offer no
    // choice of every workspace.
    let workspace = match current_chat(&scope.workspace) {
        Some(chat) if !scope.all => Some(chat),
        _ if names.is_empty() && ids.is_empty() => ui::select_workspace_filter(ctx, scope).await?,
        _ => Some(ui::select_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await?),
    };
    if let Some(workspace) = &workspace {
        check_selected(ctx, workspace, &names, &ids).await?;
    }
    if !yes && let Some(action) = confirmation(ctx, workspace.as_deref()).await? {
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let deleted = client::pages::<AttachedFile>(
        &ctx.paths,
        FileMethod::RemoveFiles {
            workspace,
            names,
            ids,
        },
    )
    .await?;
    let text = match deleted.len() {
        0 => "No files deleted".to_owned(),
        1 => "Deleted 1 file".to_owned(),
        n => format!("Deleted {n} files"),
    };
    ctx.emit(&text, &deleted)?;
    Ok(0)
}

/// Refuse names and records the owner's files do not have before asking
/// anything; the daemon checks again as it deletes.
async fn check_selected(
    ctx: &Context,
    workspace: &str,
    names: &[String],
    ids: &[i64],
) -> Result<()> {
    if names.is_empty() && ids.is_empty() {
        return Ok(());
    }
    let method = FileMethod::ListFiles {
        workspace: workspace.to_owned(),
    };
    let files = client::pages::<AttachedFile>(&ctx.paths, method).await?;
    let missing = crate::daemon::workspace_files::missing_selection(&files, names, ids);
    ensure!(
        missing.is_empty(),
        "no attached file {}",
        missing.join(", ")
    );
    Ok(())
}

/// What deleting the files of `workspace`, a workspace or chat, or of every
/// workspace, asks the user to confirm, if anything.
async fn confirmation(ctx: &Context, workspace: Option<&str>) -> Result<Option<String>> {
    let Some(workspace) = workspace else {
        return Ok(Some(
            "Delete the attached files of every workspace without a running agent or command, \
             and of every ended chat"
                .into(),
        ));
    };
    Ok(running_owner(ctx, workspace)
        .await?
        .map(|name| format!("An agent or command runs in {name}. Delete its attached files")))
}

/// The name of the workspace where an agent or command runs, or of the
/// running chat, that `selector` names.
async fn running_owner(ctx: &Context, selector: &str) -> Result<Option<String>> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    if let Some(listed) = workspaces
        .iter()
        .find(|listed| listed.id == selector || listed.name == selector)
    {
        return Ok(listed.running.then(|| listed.name.clone()));
    }
    Ok(client::chats(&ctx.paths)
        .await?
        .into_iter()
        .find(|chat| chat.id == selector || chat.name == selector)
        .map(|chat| chat.name))
}

/// The workspace or chat whose files a command acts on: the one `explicit`
/// names, else the chat this process runs under, else the current workspace
/// or one picked.
async fn select_owner(ctx: &Context, explicit: Option<String>) -> Result<String> {
    match current_chat(&explicit) {
        Some(chat) => Ok(chat),
        None => ui::select_workspace(ctx, explicit, Fallback::CurrentDirectory).await,
    }
}

/// The chat this process runs under, outside any workspace scope, when no
/// workspace or chat is `explicit`.
fn current_chat(explicit: &Option<String>) -> Option<String> {
    if explicit.is_some() || crate::env::is_scoped() {
        return None;
    }
    crate::env::chat()
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
