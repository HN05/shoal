//! Asking each selected space's daemon to resume its agent, and gathering
//! what each did into one report: the restores and launches queued or
//! started, the stopped commands no agent receives, the turns continued
//! and the spaces it failed for.
use anyhow::Result;

use super::{Chosen, Resuming};
use crate::client::output::{
    agents::Agent, restore::Reopened, resume::Queued, resume::StoppedCommands,
};
use crate::{
    cli::{context::Context, ui},
    model::{AgentResume, ResumeOutcome, Space},
};
use shoal_protocol::space_names::SpaceSelector;

/// Resume the agent of each space `resuming` lists as `resume` says, one
/// request per space, going on after one fails, and report what each did
/// with the spaces reopened for it; a space with nothing to resume fails
/// when it was named or picked.
pub(super) async fn resume_each(
    ctx: &Context,
    resuming: Resuming,
    resume: &AgentResume,
) -> Result<Queued> {
    let named = resuming.chosen == Chosen::Named;
    let mut report = Queued {
        reopened: resuming.reopened,
        ..Queued::default()
    };
    let listed = crate::client::spaces::list(&ctx.paths).await?;
    for selector in &resuming.spaces {
        let space = listed.find(selector);
        let name = space
            .as_ref()
            .map_or_else(|| selector.to_string(), |space| space.name.to_string());
        let request = crate::client::agents::resume(selector.clone(), resume.clone());
        match request.send(&ctx.paths).await {
            Ok(ResumeOutcome::Nothing { reason }) if named => {
                let error = anyhow::anyhow!(reason);
                report
                    .failed
                    .push(ui::failure(ctx, "resume", &name, &error));
            }
            Ok(outcome) => add(ctx, &mut report, space.as_ref(), outcome),
            Err(error) => {
                keep_saved(&mut report.reopened, selector);
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

/// Add what the resume of `space` did, its `outcome`, to `report`.
fn add(ctx: &Context, report: &mut Queued, space: Option<&Space>, outcome: ResumeOutcome) {
    let Some(space) = space else {
        return;
    };
    match outcome {
        ResumeOutcome::Continued { agent } => report.continued.push(Agent {
            space: space.name.clone(),
            agent,
        }),
        ResumeOutcome::Queued { entries, commands } => {
            report.queued.extend(entries);
            add_commands(ctx, report, space, commands);
        }
        ResumeOutcome::Commands { commands } => add_commands(ctx, report, space, commands),
        ResumeOutcome::Nothing { .. } => {}
    }
}

/// Add the stopped `commands` of `space`, which no agent receives, to
/// `report`, listing them in text output.
fn add_commands(ctx: &Context, report: &mut Queued, space: &Space, commands: Vec<Vec<String>>) {
    if commands.is_empty() {
        return;
    }
    super::reports::stopped_commands(ctx, (space.number, &space.name), &commands);
    report.commands.push(StoppedCommands {
        workspace: space.name.clone(),
        commands,
    });
}
