//! Asking each selected space's daemon to resume its agent, and gathering
//! what each did into one report: the restores and launches queued or
//! started, here or on another joined machine, the stopped commands no
//! agent receives, the turns continued, the spaces left and those it
//! failed for.
use anyhow::Result;

use super::{Chosen, Resuming, targets::Target};
use crate::client::output::{
    agents::Agent,
    restore::Reopened,
    resume::{Queued, QueuedElsewhere, StoppedCommands},
};
use crate::{
    cli::{context::Context, ui},
    model::{AgentResume, ResumeOutcome},
};
use shoal_protocol::space_names::SpaceSelector;

/// Resume the agent of each space `resuming` lists as `resume` says, one
/// request per space, going on after one fails, and report what each did
/// with the spaces reopened for it and those it left; a space with nothing
/// to resume fails when it was named or picked.
pub(super) async fn resume_each(
    ctx: &Context,
    resuming: Resuming,
    resume: &AgentResume,
) -> Result<Queued> {
    let named = resuming.chosen == Chosen::Named;
    let mut report = Queued {
        reopened: resuming.reopened,
        skipped: resuming.skipped,
        ..Queued::default()
    };
    for space in &resuming.spaces {
        let name = space.shown();
        let request = crate::client::agents::resume(space.selector.clone(), resume.clone());
        match request.send(&ctx.paths).await {
            Ok(ResumeOutcome::Nothing { reason }) if named => {
                let error = anyhow::anyhow!(reason);
                report
                    .failed
                    .push(ui::failure(ctx, "resume", &name, &error));
            }
            Ok(outcome) => add(ctx, &mut report, space, outcome),
            Err(error) => {
                keep_saved(&mut report.reopened, &space.selector);
                (report.failed).push(ui::failure(ctx, "resume", &name, &error));
            }
        }
    }
    Ok(report)
}

/// Mark the space `selector` names, when `shoal restore` reopened it for
/// this resume, as keeping its saved agents, which its resume left there,
/// so its report names the `shoal resume` that resumes them.
fn keep_saved(reopened: &mut [Reopened], selector: &SpaceSelector) {
    let opened = reopened
        .iter_mut()
        .find(|opened| selector.given_id() == Some(&opened.space.id));
    if let Some(opened) = opened {
        opened.agents_saved = true;
    }
}

/// Add what the resume of `space` did, its `outcome`, to `report`: what
/// another machine queued apart from this machine's queue.
fn add(ctx: &Context, report: &mut Queued, space: &Target, outcome: ResumeOutcome) {
    let Some((_, name)) = &space.space else {
        return;
    };
    let machine = space.machine_name();
    match outcome {
        ResumeOutcome::Continued { agent } => report.continued.push(Agent {
            space: name.clone(),
            agent,
            machine,
        }),
        ResumeOutcome::Queued { entries, commands } => {
            match machine {
                Some(machine) if !entries.is_empty() => report.elsewhere.push(QueuedElsewhere {
                    space: name.clone(),
                    machine,
                    queued: entries,
                }),
                _ => report.queued.extend(entries),
            }
            add_commands(ctx, report, space, commands);
        }
        ResumeOutcome::Commands { commands } => add_commands(ctx, report, space, commands),
        ResumeOutcome::Nothing { .. } => {}
    }
}

/// Add the stopped `commands` of `space`, which no agent receives, to
/// `report`, listing them in text output.
fn add_commands(ctx: &Context, report: &mut Queued, space: &Target, commands: Vec<Vec<String>>) {
    let Some((number, name)) = &space.space else {
        return;
    };
    if commands.is_empty() {
        return;
    }
    let machine = space.machine_name();
    super::reports::stopped_commands(ctx, (*number, name, machine.as_deref()), &commands);
    report.commands.push(StoppedCommands {
        workspace: name.clone(),
        commands,
        machine,
    });
}
