//! Acting on several items at once: a picker where Tab selects each and
//! Ctrl-A all of them, one confirmation for all of them, and a report of
//! what happened to each.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{Entries, confirm, entry_id, picker_output};
use crate::cli::{
    context::Context,
    output::{Palette, Style},
};

/// Tab selects an entry and moves on to the next, which the bottom-up list
/// shows above it; Shift-Tab moves back.
const NEXT: &str = "tab:toggle+up,btab:toggle+down";

/// Ctrl-A selects every listed entry, or clears the selection once all of
/// them are selected.
const SELECT_ALL: &str = "ctrl-a:transform:[ \"$FZF_SELECT_COUNT\" -lt \"$FZF_MATCH_COUNT\" ] && echo select-all || echo deselect-all";

/// Choose any number of entries, with Tab selecting each and Ctrl-A all,
/// returning their ids in list order. Enter without a selection chooses the
/// highlighted entry.
pub fn pick_many(ctx: &Context, prompt: &str, entries: Entries) -> Result<Vec<String>> {
    let extra = [
        "--multi".to_owned(),
        "--marker=✓".to_owned(),
        "--bind".to_owned(),
        NEXT.to_owned(),
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

/// Ask once whether to go on with `action` on every item of `losses`, as
/// `(item, lines)`, listing what each item that loses something loses, under
/// its name when the action covers several. `flag` skips the question.
pub fn confirm_losses(
    ctx: &Context,
    action: &str,
    losses: &[(String, Vec<String>)],
    flag: &str,
) -> Result<bool> {
    confirm(ctx, &losses_text(action, losses), flag)
}

fn losses_text(action: &str, losses: &[(String, Vec<String>)]) -> String {
    let several = losses.len() > 1;
    let mut text = action.to_owned();
    for (item, lines) in losses.iter().filter(|(_, lines)| !lines.is_empty()) {
        if several {
            text.push_str(&format!("\n{item}:"));
        }
        for line in lines {
            text.push('\n');
            if several {
                text.push_str("  ");
            }
            text.push_str(line);
        }
    }
    text
}

/// What a command did to each of several items: a line per item as it goes,
/// then one JSON object naming the items under `done_key` and the failures
/// under `failed`.
pub struct Report {
    done_key: &'static str,
    done: Vec<Value>,
    failed: Vec<Value>,
}

impl Report {
    pub fn new(done_key: &'static str) -> Self {
        Self {
            done_key,
            done: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Record an item the command acted on, printing `message`.
    pub fn done(&mut self, ctx: &Context, message: &str, value: Value) {
        if !ctx.json {
            outln!("{}", Palette::stdout(false).paint(Style::Success, message));
        }
        self.done.push(value);
    }

    /// Record an item the command failed on, which does not stop the others.
    pub fn failed(&mut self, ctx: &Context, item: &str, error: &anyhow::Error) {
        eprintln!(
            "{} {item}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:")
        );
        self.failed
            .push(json!({"workspace": item, "error": format!("{error:#}")}));
    }

    /// Print the JSON object, exiting 1 when any item failed.
    pub fn finish(self, ctx: &Context) -> Result<i32> {
        if ctx.json {
            outln!(
                "{}",
                json!({self.done_key: self.done, "failed": self.failed})
            );
        }
        Ok(if self.failed.is_empty() { 0 } else { 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loss(item: &str, lines: &[&str]) -> (String, Vec<String>) {
        (
            item.into(),
            lines.iter().map(|line| (*line).into()).collect(),
        )
    }

    #[test]
    fn losses_name_their_item_only_when_the_action_covers_several() {
        assert_eq!(
            losses_text("Remove workspace: a", &[loss("a", &["Deletes: 1 commit"])]),
            "Remove workspace: a\nDeletes: 1 commit"
        );
        assert_eq!(
            losses_text(
                "Remove 3 workspaces: a, b, c",
                &[
                    loss("a", &["Holds:", "  review"]),
                    loss("b", &[]),
                    loss("c", &["Deletes: uncommitted changes"]),
                ]
            ),
            "Remove 3 workspaces: a, b, c\na:\n  Holds:\n    review\nc:\n  Deletes: uncommitted changes"
        );
    }
}
