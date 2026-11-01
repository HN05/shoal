//! Acting on several items at once: a picker where Tab selects each and
//! Ctrl-A all of them, one confirmation for all of them, and a report of
//! what happened to each.
use anyhow::{Result, ensure};

use super::{Entries, confirm, entry_id, picker_output};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    client::output::{Output, batch::Failure},
};

/// Tab selects an entry and moves on to the next, which the bottom-up list
/// shows above it; Shift-Tab moves back.
const NEXT: &str = "tab:toggle+up,btab:toggle+down";

/// Ctrl-A selects every matching entry, or clears them once all of them
/// are selected. fzf lists the selected entries, which may include some
/// the search hides, in `{+f}` and the matching ones in `{*f}`; without a
/// selection, `{+f}` holds the highlighted entry instead.
const SELECT_ALL: &str = "ctrl-a:transform:[ \"$FZF_SELECT_COUNT\" -gt 0 ] && awk 'FNR == NR { selected[$0]; next } !($0 in selected) { missing = 1 } END { print missing ? \"select-all\" : \"deselect-all\" }' {+f} {*f} || echo select-all";

/// Choose any number of entries, with Tab selecting each and Ctrl-A all,
/// returning their ids in list order. Enter without a selection chooses the
/// highlighted entry.
pub fn pick_many(ctx: &Context, prompt: &str, entries: Entries) -> Result<Vec<String>> {
    let extra = [
        "--multi".to_owned(),
        "--with-shell=sh -c".to_owned(),
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

/// Print that the command, doing `verb`, failed on `workspace`, and return
/// the failure as the JSON of a batch lists it.
pub fn failure(ctx: &Context, verb: &str, workspace: &str, error: &anyhow::Error) -> Failure {
    eprintln!(
        "{} cannot {verb} {workspace}: {error:#}",
        Palette::stderr(ctx.json).paint(Style::Error, "error:")
    );
    failure_value(workspace, error)
}

fn failure_value(workspace: &str, error: &anyhow::Error) -> Failure {
    Failure {
        workspace: workspace.to_owned(),
        error: format!("{error:#}"),
    }
}

/// What a command did to each of several items: a line per item as it goes,
/// then a summary, or with `--json` the output naming the items it acted on
/// and those it failed on.
pub struct Report<T> {
    /// What the command does, as in `cannot remove first`.
    verb: &'static str,
    done: Vec<T>,
    failed: Vec<Failure>,
}

impl<T> Report<T> {
    pub fn new(verb: &'static str) -> Self {
        Self {
            verb,
            done: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Record an item the command acted on, printing `message`.
    pub fn done(&mut self, ctx: &Context, message: &str, value: T) {
        if !ctx.json {
            outln!("{}", Palette::stdout(false).paint(Style::Success, message));
        }
        self.done.push(value);
    }

    /// Record a workspace the command failed on, which does not stop the
    /// others.
    pub fn failed(&mut self, ctx: &Context, workspace: &str, error: &anyhow::Error) {
        self.failed.push(failure(ctx, self.verb, workspace, error));
    }

    /// Record a workspace the command failed on, printing `message`, which
    /// names the failure in the command's own words, instead of an error line.
    pub fn failed_saying(
        &mut self,
        ctx: &Context,
        message: &str,
        workspace: &str,
        error: &anyhow::Error,
    ) {
        if !ctx.json {
            eprintln!("{}", Palette::stderr(false).paint(Style::Error, message));
        }
        self.failed.push(failure_value(workspace, error));
    }

    /// How many items the command acted on, and how many it failed on.
    pub fn counts(&self) -> (usize, usize) {
        (self.done.len(), self.failed.len())
    }

    /// Print `summary` unless it is empty, or with `--json` what `output`
    /// makes of the items acted on and failed on, exiting 1 when any failed.
    pub fn finish<O: Output>(
        self,
        ctx: &Context,
        summary: &str,
        output: impl FnOnce(Vec<T>, Vec<Failure>) -> O,
    ) -> Result<i32> {
        let code = if self.failed.is_empty() { 0 } else { 1 };
        if ctx.json {
            ctx.show(&output(self.done, self.failed), |_| {})?;
        } else if !summary.is_empty() {
            outln!("{summary}");
        }
        Ok(code)
    }
}

#[cfg(test)]
mod tests;
