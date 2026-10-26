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
        "--marker=✓ ".to_owned(),
        "--color=marker:green:bold".to_owned(),
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
/// then a summary, or with `--json` one object naming the items under
/// `done_key` and the failures under `failed`.
pub struct Report {
    done_key: &'static str,
    /// What the command does, as in `cannot remove first`.
    verb: &'static str,
    done: Vec<Value>,
    failed: Vec<Value>,
    fields: serde_json::Map<String, Value>,
}

impl Report {
    pub fn new(done_key: &'static str, verb: &'static str) -> Self {
        Self {
            done_key,
            verb,
            done: Vec::new(),
            failed: Vec::new(),
            fields: serde_json::Map::new(),
        }
    }

    /// Record an item the command acted on, printing `message`.
    pub fn done(&mut self, ctx: &Context, message: &str, value: Value) {
        if !ctx.json {
            outln!("{}", Palette::stdout(false).paint(Style::Success, message));
        }
        self.done.push(value);
    }

    /// Record a workspace the command failed on, which does not stop the
    /// others.
    pub fn failed(&mut self, ctx: &Context, workspace: &str, error: &anyhow::Error) {
        eprintln!(
            "{} cannot {} {workspace}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:"),
            self.verb
        );
        self.failed
            .push(json!({"workspace": workspace, "error": format!("{error:#}")}));
    }

    /// How many items the command acted on, and how many it failed on.
    pub fn counts(&self) -> (usize, usize) {
        (self.done.len(), self.failed.len())
    }

    /// Add `key` to the JSON object.
    pub fn field(&mut self, key: &str, value: Value) {
        self.fields.insert(key.to_owned(), value);
    }

    /// Print `summary` unless it is empty, or the JSON object, exiting 1 when
    /// any item failed.
    pub fn finish(mut self, ctx: &Context, summary: &str) -> Result<i32> {
        let code = if self.failed.is_empty() { 0 } else { 1 };
        self.fields
            .insert(self.done_key.to_owned(), self.done.into());
        self.fields.insert("failed".to_owned(), self.failed.into());
        if ctx.json {
            outln!("{}", Value::Object(self.fields));
        } else if !summary.is_empty() {
            outln!("{summary}");
        }
        Ok(code)
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
