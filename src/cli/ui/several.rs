//! Acting on several items at once: a picker where Tab selects each and
//! Ctrl-A all of them.
use anyhow::{Result, ensure};

use super::{Entries, entry_id, picker_output};

/// Ctrl-A selects every listed entry, or clears the selection once all of
/// them are selected.
const SELECT_ALL: &str = "ctrl-a:transform:[ \"$FZF_SELECT_COUNT\" -lt \"$FZF_MATCH_COUNT\" ] && echo select-all || echo deselect-all";

/// Choose any number of entries, with Tab selecting each and Ctrl-A all,
/// returning their ids in list order. Enter without a selection chooses the
/// highlighted entry.
pub fn pick_many(
    ctx: &crate::cli::context::Context,
    prompt: &str,
    entries: Entries,
) -> Result<Vec<String>> {
    let extra = [
        "--multi".to_owned(),
        "--bind".to_owned(),
        SELECT_ALL.to_owned(),
        "--header".to_owned(),
        "Tab: select   Ctrl-A: all   Enter: confirm".to_owned(),
    ];
    let output = picker_output(ctx, prompt, &entries, &extra)?;
    let ids: Vec<String> = output.lines().map(entry_id).collect();
    ensure!(
        ids.iter()
            .all(|id| entries.iter().any(|entry| &entry.0 == id)),
        "picker returned an unknown item"
    );
    Ok(ids)
}
