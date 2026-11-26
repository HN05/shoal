//! `shoal changes`, `claim` and `unclaim`.
use anyhow::Result;
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::{
    changes::{Change, ChangeNumber},
    protocol::ChangeFilter,
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Cell, Fallback},
    },
    client::changes,
};

pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    thread: Option<ThreadNumber>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = match thread {
        Some(_) => None,
        None => Some(ui::select_space(ctx, space, Fallback::OwnSpace).await?),
    };
    let filter = ChangeFilter {
        space,
        thread,
        since: None,
    };
    let changes = changes::list(&ctx.paths, filter).await?;
    ctx.show(&changes, |changes| {
        for line in rows(changes, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
        if changes.is_empty() {
            outln!("No changes");
        }
    })?;
    Ok(0)
}

pub(in crate::cli::commands) async fn claim(
    ctx: &Context,
    numbers: Vec<ChangeNumber>,
    thread: Option<ThreadNumber>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = own_space(ctx, space, &numbers).await?;
    let claimed = changes::claim(&ctx.paths, space, numbers, thread).await?;
    let message = match thread {
        Some(thread) => format!("Claimed {} for thread {thread}.", labels(&claimed)),
        None => format!("Claimed {}.", labels(&claimed)),
    };
    ctx.emit(&message, &claimed)?;
    Ok(0)
}

pub(in crate::cli::commands) async fn unclaim(
    ctx: &Context,
    numbers: Vec<ChangeNumber>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = own_space(ctx, space, &numbers).await?;
    let unclaimed = changes::unclaim(&ctx.paths, space, numbers).await?;
    ctx.emit(&format!("Unclaimed {}.", labels(&unclaimed)), &unclaimed)?;
    Ok(0)
}

/// The space whose changes the bare numbers among `numbers` name: `space`,
/// else the caller's own or the current workspace; none without a bare
/// number.
pub(in crate::cli::commands) async fn own_space(
    ctx: &Context,
    space: Option<SpaceSelector>,
    numbers: &[ChangeNumber],
) -> Result<Option<SpaceSelector>> {
    let bare = numbers
        .iter()
        .any(|number| matches!(number, ChangeNumber::Own(_)));
    match (space, bare) {
        (Some(space), _) => Ok(Some(space)),
        (None, true) => Ok(Some(ui::select_space(ctx, None, Fallback::OwnSpace).await?)),
        (None, false) => Ok(None),
    }
}

pub(in crate::cli::commands) fn labels(changes: &[Change]) -> String {
    let own = one_space(changes);
    let labels: Vec<String> = changes.iter().map(|change| change.label(own)).collect();
    match labels.len() {
        1 => format!("change {}", labels[0]),
        _ => format!("changes {}", labels.join(", ")),
    }
}

pub(in crate::cli::commands) fn rows(changes: &[Change], palette: Palette) -> Vec<String> {
    let own = one_space(changes);
    let cells: Vec<[Cell; 5]> = (changes.iter())
        .map(|change| {
            let size = match change.size {
                Some(bytes) => format!("{bytes} bytes"),
                None => format!("+{} −{}", change.added, change.removed),
            };
            let thread = change
                .thread
                .map(|thread| thread.to_string())
                .unwrap_or_default();
            [
                (change.label(own), Some(Style::Heading)),
                (change.author.to_string(), Some(Style::Muted)),
                (change.place(), None),
                (size, None),
                (thread, Some(Style::Muted)),
            ]
        })
        .collect();
    ui::aligned(cells, palette)
}

/// The number of the space all of `changes` are in, whose changes their
/// bare numbers name; none when they are in several.
fn one_space(changes: &[Change]) -> Option<SpaceNumber> {
    let first = changes.first()?.space.number;
    changes
        .iter()
        .all(|change| change.space.number == first)
        .then_some(first)
        .flatten()
}
