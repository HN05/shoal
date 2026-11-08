//! `shoal progress`: show or set where a space's agent is in its work, as a
//! status line and the steps of its plan.
use std::io::Read;

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::{
        progress::{self, PlanStep, Progress, ProgressChange, StepState},
        space_names::SpaceSelector,
    },
};

/// Set the status line to `status`, which clears it when empty, and with
/// `steps` replace the steps with the task list on stdin; with neither,
/// show the progress of the space `space` names, else the current one.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    status: Option<String>,
    steps: bool,
) -> Result<i32> {
    let changes = changes(status, steps)?;
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let space = SpaceSelector::parse(&space);
    let progress = match changes.is_empty() {
        true => progress::get(&ctx.paths, space).await?,
        false => apply(ctx, space, changes).await?,
    };
    ctx.show(&progress, |progress| {
        render(progress.as_ref(), Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

/// The changes `status` and `steps` ask for, the steps first. A status
/// line that is not a valid line is refused before anything is read,
/// picked or changed.
fn changes(status: Option<String>, steps: bool) -> Result<Vec<ProgressChange>> {
    if let Some(status) = status.as_deref().filter(|status| !status.is_empty()) {
        progress::validate_line("status lines", status)?;
    }
    let mut changes = Vec::new();
    if steps {
        changes.push(ProgressChange::Steps(read_steps()?));
    }
    if let Some(status) = status {
        changes.push(ProgressChange::Status(
            Some(status).filter(|status| !status.is_empty()),
        ));
    }
    Ok(changes)
}

/// Apply each of `changes` in order, returning the progress after the last.
async fn apply(
    ctx: &Context,
    space: SpaceSelector,
    changes: Vec<ProgressChange>,
) -> Result<Option<Progress>> {
    let mut progress = None;
    for change in changes {
        progress = progress::set(&ctx.paths, space.clone(), change).await?;
    }
    Ok(progress)
}

/// The steps of the markdown task list on stdin.
fn read_steps() -> Result<Vec<PlanStep>> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("read the task list from stdin")?;
    task_list(&text)
}

/// The steps of a markdown task list, one `- [ ] name` or `- [x] name` per
/// line; blank lines are skipped and any other line is refused.
pub(in crate::cli) fn task_list(text: &str) -> Result<Vec<PlanStep>> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(at, line)| task(line).with_context(|| format!("line {}", at + 1)))
        .collect()
}

/// The step one task list line names.
fn task(line: &str) -> Result<PlanStep> {
    let item = line.trim_start();
    let item = (item.strip_prefix("- ").or_else(|| item.strip_prefix("* ")))
        .context("expected a task such as `- [ ] name` or `- [x] name`")?;
    let (state, name) = match item.get(..3) {
        Some("[ ]") => (StepState::Pending, &item[3..]),
        Some("[x]" | "[X]") => (StepState::Done, &item[3..]),
        _ => bail!("expected `[ ]` or `[x]` after the list marker"),
    };
    let name = name.trim();
    progress::validate_line("step names", name)?;
    Ok(PlanStep {
        name: name.to_owned(),
        state,
        key: None,
    })
}

/// `progress` as `shoal progress` shows it: the status line, then the
/// numbered steps.
fn render(progress: Option<&Progress>, palette: Palette) {
    let Some(progress) = progress else {
        outln!("No progress reported.");
        return;
    };
    if let Some(status) = &progress.status {
        outln!("{status}");
    }
    for line in step_lines(progress, palette) {
        outln!("{line}");
    }
}

/// `progress` as a space's status shows it: how many steps are done, then
/// the status line and the numbered steps.
pub(in crate::cli) fn render_section(progress: Option<&Progress>, palette: Palette) {
    let Some(progress) = progress else {
        outln!("Progress:      none");
        return;
    };
    outln!("Progress:      {}", summary(progress));
    if let Some(status) = &progress.status {
        outln!("  {status}");
    }
    for line in step_lines(progress, palette) {
        outln!("  {line}");
    }
}

/// How many of the steps are done, as `3/5 steps`.
fn summary(progress: &Progress) -> String {
    match progress.steps.len() {
        0 => "no steps".to_owned(),
        total => format!("{}/{total} steps", progress.done()),
    }
}

/// Each step as `2. [x] name`, the current one marked.
fn step_lines(progress: &Progress, palette: Palette) -> Vec<String> {
    let width = progress.steps.len().to_string().len();
    (progress.steps.iter().enumerate())
        .map(|(at, step)| {
            let check = if step.done { "[x]" } else { "[ ]" };
            let line = format!("{:>width$}. {check} {}", at + 1, step.name);
            match progress.current == Some(at) {
                true => format!("{}  {}", palette.paint(Style::Heading, &line), "(current)"),
                false => line,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
