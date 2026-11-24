//! The commands stopped with a workspace's agents: one restore claims them,
//! then reports them to the user or hands them to the restored agent.
use anyhow::Result;

use shoal_core::execution::recovery::{self, RecoveryClaim, SavedCommand, consume};

use super::Resuming;
use crate::{client::output::resume::StoppedCommands, model::Space};

/// Only one restore reports a command; another holding its lock skips it.
pub fn claim(commands: &mut Vec<SavedCommand>) -> Vec<RecoveryClaim> {
    let mut claims = Vec::new();
    commands.retain(|command| match recovery::claim(&command.path) {
        Ok(file) => {
            claims.push(file);
            true
        }
        Err(_) => false,
    });
    claims
}

/// Report the stopped `commands` of `space` that no other restore holds
/// to the user, as [`report`] does, claiming them meanwhile.
pub fn report_claimed(
    resuming: &impl Resuming,
    space: &Space,
    commands: &mut Vec<SavedCommand>,
) -> Result<StoppedCommands> {
    let _claims = claim(commands);
    report(resuming, space, commands)
}

/// Without an agent to receive them, interrupted commands go to the user
/// once, as `resuming` tells them.
fn report(
    resuming: &impl Resuming,
    space: &Space,
    commands: &[SavedCommand],
) -> Result<StoppedCommands> {
    let report = StoppedCommands {
        workspace: space.name.clone(),
        commands: commands
            .iter()
            .map(|command| command.argv.clone())
            .collect(),
    };
    resuming.stopped_commands(space, &report.commands);
    for command in commands {
        consume(&command.path)?;
    }
    Ok(report)
}

/// The first prompt of a restored session, naming the commands stopped with it.
pub fn handoff(commands: &[SavedCommand]) -> Option<String> {
    if commands.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Shoal stopped this session. These workspace commands were stopped too and were not restarted:",
    );
    for command in commands {
        text.push_str(&format!("\n- {}", crate::shell::quote(&command.argv)));
    }
    text.push_str("\nRerun the ones that are still needed.");
    Some(text)
}
