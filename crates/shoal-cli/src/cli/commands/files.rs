//! `shoal files`.
use crate::cli::ui::Acting;
use anyhow::{Result, ensure};
use shoal_protocol::model::ByKind;

use crate::cli::{
    args::SpaceFilter,
    context::Context,
    output::{Palette, Style},
    ui::{self, Fallback},
};
use shoal_protocol::files::{AttachedFile, FileSource};
use shoal_protocol::space_names::SpaceSelector;

mod menu;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Listing {
    /// A picker on a terminal, otherwise a list.
    Menu,
    /// A list.
    Print,
    /// Only where Shoal keeps them.
    Paths,
}

pub(super) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    listing: Listing,
) -> Result<i32> {
    let menu = listing == Listing::Menu && ctx.interactive_output();
    let listing_files = showing();
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&listing_files)).await?;
    let files = crate::client::files::list(space.clone())
        .send(&ctx.paths)
        .await?;
    if menu {
        return menu::choose(ctx, space.clone(), &files).await;
    }
    let paths = listing == Listing::Paths;
    ctx.show(&files, |files| {
        if paths {
            for path in files.iter().filter_map(AttachedFile::path) {
                outln!("{}", path.display());
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

/// Each file is checked before a workspace is picked.
pub(super) async fn add(
    ctx: &Context,
    space: Option<SpaceSelector>,
    files: Vec<String>,
) -> Result<i32> {
    let files = super::send::attachments(&files)?;
    let adding = adding();
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&adding)).await?;
    let added = crate::client::files::add(space.clone(), files)
        .send(&ctx.paths)
        .await?;
    ctx.show(&added, |added| {
        let now = crate::time::UnixTime::now();
        let rows = added.iter().map(|file| cells(file, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
    })?;
    Ok(0)
}

/// Asks first for every workspace, or for one where an agent or command runs,
/// unless `yes`.
pub(super) async fn remove(
    ctx: &Context,
    filter: SpaceFilter,
    names: Vec<String>,
    ids: Vec<i64>,
    yes: bool,
) -> Result<i32> {
    // Names and records belong to one workspace or chat, so they offer no
    // choice of every workspace.
    let removing = removing();
    let space = if names.is_empty() && ids.is_empty() {
        ui::select_space_filter(ctx, filter, &removing).await?
    } else {
        let fallback = Fallback::CurrentDirectory(&removing);
        Some(ui::select_space(ctx, filter.space, fallback).await?)
    };
    if let Some(space) = &space {
        check_selected(ctx, space, &names, &ids).await?;
    }
    if !yes && let Some(action) = confirmation(ctx, space.as_ref()).await? {
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let deleted = crate::client::files::remove(space, names, ids)
        .send(&ctx.paths)
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
    space: &SpaceSelector,
    names: &[String],
    ids: &[i64],
) -> Result<()> {
    if names.is_empty() && ids.is_empty() {
        return Ok(());
    }
    let files = crate::client::files::list(space.clone())
        .send(&ctx.paths)
        .await?;
    let missing = shoal_protocol::files::missing_selection(&files, names, ids);
    ensure!(
        missing.is_empty(),
        "no attached file {}",
        missing.join(", ")
    );
    Ok(())
}

async fn confirmation(ctx: &Context, space: Option<&SpaceSelector>) -> Result<Option<String>> {
    let Some(space) = space else {
        return Ok(Some(
            "Delete the attached files of every workspace without a running agent or command, \
             and of every removed space"
                .into(),
        ));
    };
    Ok(running_space(ctx, space)
        .await?
        .map(|name| format!("An agent or command runs in {name}. Delete its attached files")))
}

async fn running_space(ctx: &Context, selector: &SpaceSelector) -> Result<Option<String>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    Ok(
        match spaces.find(selector).map(crate::model::Space::by_kind) {
            Some(ByKind::Workspace(workspace)) => {
                workspace.running.then(|| workspace.name.to_string())
            }
            Some(ByKind::Chat(chat)) => chat.running.then(|| chat.name.into()),
            None => None,
        },
    )
}

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
        (file.copy.label(), Some(Style::Muted)),
    ]
}

fn source_label(source: FileSource) -> &'static str {
    match source {
        FileSource::Message => "message",
        FileSource::Added => "added",
        FileSource::Result => "result",
        FileSource::Alert => "alert",
        FileSource::Thread => "thread",
        FileSource::Handoff => "handoff",
    }
}

pub(super) fn showing() -> Acting {
    Acting::request(crate::client::files::list)
}

/// The files decide nothing of which spaces it reaches.
pub(super) fn adding() -> Acting {
    Acting::request(|space| crate::client::files::add(space, Vec::new()))
}

pub(super) fn removing() -> Acting {
    Acting::request(|space| crate::client::files::remove(Some(space), Vec::new(), Vec::new()))
}
