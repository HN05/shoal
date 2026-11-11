//! Choosing a place in an ordered list: in front of one of its items, or
//! after the last.
use anyhow::{Context as _, Result};

use super::{Entries, pick};
use crate::cli::context::Context;

/// Where a picked place puts an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In front of the item with this id.
    Before(String),
    Last,
}

/// The id of the row that places an item last; item ids are listed by
/// index, so it cannot collide with one.
const LAST: &str = "last";

/// Choose a place among `entries`, in their order: the chosen entry's, which
/// moves it and those after it back, or the last row's.
pub fn pick_place(ctx: &Context, prompt: &str, entries: Entries) -> Result<Place> {
    let rows = place_rows(&entries);
    let id = pick(ctx, prompt, rows)?;
    place_of(&entries, &id)
}

/// The picker rows for `entries`: one per entry by index, then the last place.
fn place_rows(entries: &Entries) -> Entries {
    entries
        .iter()
        .enumerate()
        .map(|(index, (_, label))| (index.to_string(), label.clone()))
        .chain(std::iter::once((LAST.to_owned(), "Last".to_owned())))
        .collect()
}

/// The place the picker row `id` stands for.
fn place_of(entries: &Entries, id: &str) -> Result<Place> {
    if id == LAST {
        return Ok(Place::Last);
    }
    let index: usize = id.parse().context("picker returned an invalid place")?;
    let (id, _) = entries
        .get(index)
        .context("picker returned an unknown place")?;
    Ok(Place::Before(id.clone()))
}

#[cfg(test)]
mod tests;
