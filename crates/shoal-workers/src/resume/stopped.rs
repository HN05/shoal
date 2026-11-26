//! Restoring stopped agents through their space's daemon.
use anyhow::Result;
use shoal_protocol::model::{AgentResume, ResumeOutcome};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transfer::StoppedExecutions;

use super::Resuming;
use crate::{
    client::{
        connection::CommandPaths,
        output::resume::{Queued, StoppedCommands},
    },
    model::Space,
};

/// Have the daemon queue the restores of the agents and last sessions
/// among `stopped`, of each space it still lists, going on after one
/// fails, which `resuming` tells; their spaces' stopped commands go to the
/// restored agents, or to the user when none restores outside a waiting
/// wrapper, and those of a space without one stay for `shoal resume`.
pub async fn restore(
    paths: &CommandPaths,
    stopped: &StoppedExecutions,
    resuming: &impl Resuming,
) -> Result<Queued> {
    let mut report = Queued::default();
    let listed = crate::client::spaces::list(paths).await?;
    for (space, executions) in stopped.iter().filter(|(_, ids)| !ids.is_empty()) {
        let Some(space) = listed.find(&SpaceSelector::id(space)) else {
            continue;
        };
        let resume = AgentResume {
            executions: executions.clone(),
            env: crate::terminal::start_environment(),
            ..AgentResume::default()
        };
        resume_one(paths, &space, resume, resuming, &mut report).await;
    }
    Ok(report)
}

/// Have the daemon resume what `resume` names of `space`, and add what it
/// did to `report`. `resuming` tells of the stopped commands it gave back
/// and of a failure.
pub async fn resume_one(
    paths: &CommandPaths,
    space: &Space,
    resume: AgentResume,
    resuming: &impl Resuming,
    report: &mut Queued,
) {
    let request = crate::client::agents::resume(SpaceSelector::id(&space.id), resume);
    match request.send(paths).await {
        Ok(ResumeOutcome::Queued { entries, commands }) => {
            report.queued.extend(entries);
            add_commands(resuming, report, space, commands);
        }
        Ok(ResumeOutcome::Commands { commands }) => add_commands(resuming, report, space, commands),
        // Only a resume without executions continues a turn.
        Ok(ResumeOutcome::Continued { .. } | ResumeOutcome::Nothing { .. }) => {}
        Err(error) => report.failed.push(resuming.failed(&space.name, &error)),
    }
}

/// Add the stopped `commands` of `space`, which no agent receives, to
/// `report`, telling of them as `resuming` does.
fn add_commands(
    resuming: &impl Resuming,
    report: &mut Queued,
    space: &Space,
    commands: Vec<Vec<String>>,
) {
    if commands.is_empty() {
        return;
    }
    resuming.stopped_commands(space, &commands);
    report.commands.push(StoppedCommands {
        workspace: space.name.clone(),
        commands,
        machine: None,
    });
}
