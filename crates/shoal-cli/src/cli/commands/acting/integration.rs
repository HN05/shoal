use crate::cli::{
    args::integration::{IntegrationCommand, ShellCommand, SkillCommand},
    ui::Acting,
};

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
