use anyhow::Result;

use crate::client::output::shell::Script;
use crate::{
    cli::{
        args::integration::{IntegrationCommand, ShellCommand},
        commands::skill,
        context::Context,
    },
    shell,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: IntegrationCommand,
) -> Result<i32> {
    match command {
        IntegrationCommand::Skill { name, command } => skill::run(
            name,
            command.as_ref(),
            ctx.json,
            Some(ctx.paths.state.clone()),
        ),
        IntegrationCommand::Shell {
            command: ShellCommand::Completions { shell },
        } => {
            let script = shell::completions(shell)?;
            ctx.emit(
                &script,
                Script {
                    script: script.clone(),
                },
            )?;
            Ok(0)
        }
        IntegrationCommand::Shell {
            command: ShellCommand::Init,
        } => {
            let script = Script {
                script: shell::INIT.to_owned(),
            };
            ctx.emit(shell::INIT, script)?;
            Ok(0)
        }
        IntegrationCommand::Shell {
            command: ShellCommand::Recover { .. },
        } => unreachable!("shell recovery is handled before loading paths"),
    }
}
