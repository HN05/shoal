//! The commands stopped with a workspace's agents: one resume claims them,
//! then reports them to the user or hands them to the restored agent.
use anyhow::Result;

use crate::client::output::resume::StoppedCommands;
use crate::{
    cli::context::Context,
    execution::recovery::{self, RecoveryClaim, SavedCommand, consume},
    model::Workspace,
};

/// Only one resume reports a command; another holding its lock skips it.
pub(super) fn claim(commands: &mut Vec<SavedCommand>) -> Vec<RecoveryClaim> {
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

/// Without an agent to receive them, interrupted commands go to the user once.
pub(super) fn report(
    ctx: &Context,
    workspace: &Workspace,
    commands: &[SavedCommand],
) -> Result<StoppedCommands> {
    let report = StoppedCommands {
        workspace: workspace.name.clone(),
        commands: commands
            .iter()
            .map(|command| command.argv.clone())
            .collect(),
    };
    if !commands.is_empty() && !ctx.json {
        outln!("{}", command_list(workspace, commands));
    }
    for command in commands {
        consume(&command.path)?;
    }
    Ok(report)
}

pub(super) fn command_list(workspace: &Workspace, commands: &[SavedCommand]) -> String {
    let mut text = format!("Stopped commands in {}, not restarted:", workspace.name);
    for command in commands {
        let mut words = vec!["shoal", "run", "--workspace", &workspace.name, "--"];
        words.extend(command.argv.iter().map(String::as_str));
        text.push_str(&format!("\n  {}", crate::shell::quote(&words)));
    }
    text
}

/// The first prompt of a restored session, naming the commands stopped with it.
pub(super) fn handoff(commands: &[SavedCommand]) -> Option<String> {
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
