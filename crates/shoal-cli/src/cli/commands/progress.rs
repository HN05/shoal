//! `shoal progress`.
use std::io::Read;

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Acting, Fallback},
    },
    client::progress,
};
use shoal_protocol::{
    progress::{PlanStep, Progress, ProgressChange, RecordedBlocker, StepState},
    space_names::SpaceSelector,
};

pub(super) struct BlockerFlags {
    pub add: Option<String>,
    pub reason: Option<String>,
    pub remove: Option<String>,
    /// The step it blocks, numbered from 1; the space without one.
    pub step: Option<usize>,
}

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    status: Option<String>,
    steps: bool,
    blocker: BlockerFlags,
) -> Result<i32> {
    let blocks = blocker.add.is_some() || blocker.remove.is_some();
    let acting = reporting(status.is_some() || steps || blocks);
    let mut changes = changes(status, steps)?;
    changes.extend(blocker_change(blocker)?);
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&acting)).await?;
    let progress = match changes.is_empty() {
        true => progress::get(space).send(&ctx.paths).await?,
        false => apply(ctx, space, changes).await?,
    };
    ctx.show(&progress, |progress| {
        render(progress.as_ref(), Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

/// An invalid status line is refused before anything is read, picked or
/// changed.
fn changes(status: Option<String>, steps: bool) -> Result<Vec<ProgressChange>> {
    if let Some(status) = status.as_deref().filter(|status| !status.is_empty()) {
        shoal_protocol::progress::validate_line("status lines", status)?;
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

/// An invalid blocker or reason is refused before anything is picked or
/// changed.
fn blocker_change(flags: BlockerFlags) -> Result<Option<ProgressChange>> {
    let BlockerFlags {
        add,
        reason,
        remove,
        step,
    } = flags;
    Ok(match (add, remove) {
        (Some(add), _) => {
            let reason = reason.context("--add-blocker needs --reason")?;
            shoal_protocol::progress::validate_line("blocker reasons", &reason)?;
            Some(ProgressChange::AddBlocker {
                blocker: add.parse()?,
                step,
                reason,
            })
        }
        (None, Some(remove)) => Some(ProgressChange::RemoveBlocker {
            blocker: remove.parse()?,
            step,
        }),
        (None, None) => {
            ensure!(
                step.is_none(),
                "--step goes with --add-blocker or --remove-blocker"
            );
            None
        }
    })
}

async fn apply(
    ctx: &Context,
    space: SpaceSelector,
    changes: Vec<ProgressChange>,
) -> Result<Option<Progress>> {
    let mut progress = None;
    for change in changes {
        progress = (progress::set(space.clone(), change))
            .send(&ctx.paths)
            .await?;
    }
    Ok(progress)
}

fn read_steps() -> Result<Vec<PlanStep>> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("read the task list from stdin")?;
    task_list(&text)
}

/// One `- [ ] name` or `- [x] name` per line; blank lines are skipped and any
/// other line is refused.
pub(in crate::cli) fn task_list(text: &str) -> Result<Vec<PlanStep>> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(at, line)| task(line).with_context(|| format!("line {}", at + 1)))
        .collect()
}

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
    shoal_protocol::progress::validate_line("step names", name)?;
    Ok(PlanStep {
        name: name.to_owned(),
        state,
        key: None,
    })
}

fn render(progress: Option<&Progress>, palette: Palette) {
    let Some(progress) = progress else {
        outln!("No progress reported.");
        return;
    };
    if let Some(status) = &progress.status {
        outln!("{status}");
    }
    for line in blocker_lines(&progress.blockers) {
        outln!("{line}");
    }
    for line in step_lines(progress, palette) {
        outln!("{line}");
    }
}

pub(in crate::cli) fn render_section(progress: Option<&Progress>, palette: Palette) {
    let Some(progress) = progress else {
        outln!("Progress:      none");
        return;
    };
    outln!("Progress:      {}", summary(progress));
    if let Some(status) = &progress.status {
        outln!("  {status}");
    }
    for line in blocker_lines(&progress.blockers) {
        outln!("  {line}");
    }
    for line in step_lines(progress, palette) {
        outln!("  {line}");
    }
}

fn summary(progress: &Progress) -> String {
    let steps = match progress.steps.len() {
        0 => "no steps".to_owned(),
        total => format!("{}/{total} steps", progress.done()),
    };
    match progress.blocked() {
        true => format!("{steps}, blocked"),
        false => steps,
    }
}

fn blocker_lines(blockers: &[RecordedBlocker]) -> Vec<String> {
    (blockers.iter())
        .map(|blocker| {
            let line = format!("Blocked by {}: {}", blocker.cause(), blocker.reason);
            match &blocker.lookup_failed {
                Some(error) => format!("{line} (lookup failed: {error})"),
                None => line,
            }
        })
        .collect()
}

fn step_lines(progress: &Progress, palette: Palette) -> Vec<String> {
    let width = progress.steps.len().to_string().len();
    let mut lines = Vec::new();
    for (at, step) in progress.steps.iter().enumerate() {
        let check = if step.done { "[x]" } else { "[ ]" };
        let line = format!("{:>width$}. {check} {}", at + 1, step.name);
        lines.push(match progress.current == Some(at) {
            true => format!("{}  {}", palette.paint(Style::Heading, &line), "(current)"),
            false => line,
        });
        let indent = " ".repeat(width + 2);
        lines.extend(
            (blocker_lines(&step.blockers).into_iter()).map(|line| format!("{indent}{line}")),
        );
    }
    lines
}

pub(super) fn reporting(changes: bool) -> Acting {
    match changes {
        true => Acting::request(|space| progress::set(space, ProgressChange::Status(None))),
        false => Acting::request(progress::get),
    }
}

#[cfg(test)]
mod tests;
