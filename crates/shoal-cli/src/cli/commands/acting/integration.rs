//! Integration commands, which name no space.
use crate::cli::{
    args::integration::{IntegrationCommand, ShellCommand, SkillCommand},
    ui::Acting,
};

/// What an integration command sends for the space it acts on: none.
pub(super) fn acting(command: &IntegrationCommand) -> Option<Acting> {
    match command {
        IntegrationCommand::Skill {
            command: None | Some(SkillCommand::Install { .. }),
        }
        | IntegrationCommand::Shell {
            command:
                ShellCommand::Completions { .. } | ShellCommand::Init | ShellCommand::Recover { .. },
        } => None,
    }
}
