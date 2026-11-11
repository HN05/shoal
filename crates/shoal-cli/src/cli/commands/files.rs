//! `shoal files` lists the files attached to a workspace or chat, or on a
//! terminal opens a picker over them; `add` attaches more without a message
//! and `rm` deletes them. Under a chat's agent they act on its chat.
use anyhow::{Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::model::PresentSpace;
use crate::{
    cli::{
        args::WorkspaceScope,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::files::{AttachedFile, FileSource},
};

mod menu;

/// How `shoal files` shows the files of a space.
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
    space: Option<SpaceSelector>,
    listing: Listing,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let files = crate::client::files::list(&ctx.paths, space.clone()).await?;
    if listing == Listing::Menu && ctx.interactive_output() {
        return menu::choose(ctx, space.clone(), &files).await;
    }
    let paths = listing == Listing::Paths;
    ctx.show(&files, |files| {
        if paths {
            for file in files {
                outln!("{}", file.path.display());
            }
            return;
        }
        let now = crate::time::UnixTime::now();
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
    space: Option<SpaceSelector>,
    files: Vec<String>,
) -> Result<i32> {
    let files = super::messages::attachments(&files)?;
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let added =
        crate::client::files::add(&ctx.paths, space.clone(), files, crate::env::chat()).await?;
    ctx.show(&added, |added| {
        let now = crate::time::UnixTime::now();
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
    let in_chat = ui::current_chat(&scope.workspace).is_some() && !scope.all;
    let space = if names.is_empty() && ids.is_empty() && !in_chat {
        ui::select_space_filter(ctx, scope).await?
    } else {
        Some(ui::select_space(ctx, scope.workspace, Fallback::CurrentDirectory).await?)
    };
    if let Some(space) = &space {
        check_selected(ctx, space, &names, &ids).await?;
    }
    if !yes && let Some(action) = confirmation(ctx, space.as_ref()).await? {
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let deleted = crate::client::files::remove(&ctx.paths, space, names, ids).await?;
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
    space: &SpaceSelector,
    names: &[String],
    ids: &[i64],
) -> Result<()> {
    if names.is_empty() && ids.is_empty() {
        return Ok(());
    }
    let files = crate::client::files::list(&ctx.paths, space.clone()).await?;
    let missing = crate::client::files::missing_selection(&files, names, ids);
    ensure!(
        missing.is_empty(),
        "no attached file {}",
        missing.join(", ")
    );
    Ok(())
}

/// What deleting the files of `space`, a workspace or chat, or of every
/// workspace, asks the user to confirm, if anything.
async fn confirmation(ctx: &Context, space: Option<&SpaceSelector>) -> Result<Option<String>> {
    let Some(space) = space else {
        return Ok(Some(
            "Delete the attached files of every workspace without a running agent or command, \
             and of every removed chat"
                .into(),
        ));
    };
    Ok(running_space(ctx, space)
        .await?
        .map(|name| format!("An agent or command runs in {name}. Delete its attached files")))
}

/// The name of the workspace where an agent or command runs, or of the
/// running chat, that `selector` names.
async fn running_space(ctx: &Context, selector: &SpaceSelector) -> Result<Option<String>> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    Ok(match spaces.find(selector) {
        Some(PresentSpace::Workspace(workspace)) => {
            workspace.running.then(|| workspace.name.to_string())
        }
        Some(PresentSpace::Chat(chat)) => chat.restore.is_none().then(|| chat.name.into()),
        None => None,
    })
}

/// The file's name, title, size, how it came, when, read at `now`, when a
/// removed workspace's file is deleted, and Shoal's copy.
fn cells(file: &AttachedFile, now: crate::time::UnixTime) -> [ui::Cell; 7] {
    let expiry = file
        .expires_at
        .map(|expires_at| format!("until {}", expires_at.local_minutes()));
    [
        (file.name.clone(), Some(Style::Heading)),
        (file.title.clone().unwrap_or_default(), None),
        (ui::size_label(file.bytes), None),
        (source_label(file.source).into(), None),
        (file.created_at.local_ago(now), Some(Style::Muted)),
        (expiry.unwrap_or_default(), Some(Style::Warning)),
        (file.path.display().to_string(), Some(Style::Muted)),
    ]
}

fn source_label(source: FileSource) -> &'static str {
    match source {
        FileSource::Message => "message",
        FileSource::Added => "added",
        FileSource::Result => "result",
        FileSource::Alert => "alert",
        FileSource::Question => "question",
    }
}
