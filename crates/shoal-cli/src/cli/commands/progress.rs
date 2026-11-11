//! `shoal progress`: show or set where a space's agent is in its work, as a
//! status line, the steps of its plan and what blocks it.
use std::io::Read;

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    ai::IntegrationKind,
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::{
        progress::{self, PlanStep, Progress, ProgressChange, RecordedBlocker, StepState},
        space_names::SpaceSelector,
    },
};

/// The blocker `shoal progress` adds or removes.
pub(super) struct BlockerFlags {
    pub add: Option<String>,
    pub reason: Option<String>,
    pub remove: Option<String>,
    /// The step it blocks, numbered from 1; the space without one.
    pub step: Option<usize>,
}

/// Set the status line to `status`, which clears it when empty, with
/// `steps` replace the steps with the task list on stdin, and add or remove
/// the blocker `blocker` names; with none of them, show the progress of the
/// space `space` names, else the current one.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    status: Option<String>,
    steps: bool,
    blocker: BlockerFlags,
) -> Result<i32> {
    let mut changes = changes(status, steps)?;
    changes.extend(blocker_change(blocker)?);
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
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

/// The blocker change `flags` ask for, refused before anything is picked
/// or changed when its blocker or reason is not valid.
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
            progress::validate_line("blocker reasons", &reason)?;
            Some(ProgressChange::AddBlocker {
                blocker: super::blockers::parse(&add)?,
                step,
                reason,
            })
        }
        (None, Some(remove)) => Some(ProgressChange::RemoveBlocker {
            blocker: super::blockers::parse(&remove)?,
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

/// Report the plan change the hook event on stdin of an agent with
/// `integration` names, for the space `space` names, else the agent's own.
/// An event that changes no plan reports nothing.
pub(super) async fn report_plan(
    ctx: &Context,
    space: Option<SpaceSelector>,
    integration: IntegrationKind,
) -> Result<i32> {
    let event = super::agent_state::hook_event();
    let change = event.and_then(|event| crate::cli::agents::plan_change(integration, &event));
    let Some(change) = change.and_then(single_lines) else {
        return Ok(0);
    };
    let space = ui::select_space(ctx, space, Fallback::OwnWorkspace).await?;
    progress::set(&ctx.paths, space, change).await?;
    Ok(0)
}

/// `change` with each step name a valid line, leaving out the steps that
/// have none, so a plan tool's long or multi-line step is shown, not
/// refused; none for a created step without a name.
fn single_lines(change: ProgressChange) -> Option<ProgressChange> {
    let step = |step: PlanStep| {
        Some(PlanStep {
            name: progress::step_name(&step.name)?,
            ..step
        })
    };
    Some(match change {
        ProgressChange::Steps(steps) => {
            ProgressChange::Steps(steps.into_iter().filter_map(step).collect())
        }
        ProgressChange::StepCreated(created) => ProgressChange::StepCreated(step(created)?),
        ProgressChange::StepChanged {
            key,
            name,
            state,
            removed,
        } => ProgressChange::StepChanged {
            key,
            name: name.and_then(|name| progress::step_name(&name)),
            state,
            removed,
        },
        other @ (ProgressChange::Status(_)
        | ProgressChange::AddBlocker { .. }
        | ProgressChange::RemoveBlocker { .. }) => other,
    })
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
    for line in blocker_lines(&progress.blockers) {
        outln!("{line}");
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
    for line in blocker_lines(&progress.blockers) {
        outln!("  {line}");
    }
    for line in step_lines(progress, palette) {
        outln!("  {line}");
    }
}

/// How many of the steps are done, as `3/5 steps`, and `blocked` when the
/// space is.
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

/// Each blocker as `Blocked by issue <url>: reason`, with a failed lookup.
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

/// Each step as `2. [x] name`, the current one marked, each followed by
/// what blocks it.
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

#[cfg(test)]
mod tests;
