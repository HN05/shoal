use anyhow::Result;
use serde_json::json;

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
            ctx.emit(&script, json!({"script": script}))?;
            Ok(0)
        }
        IntegrationCommand::Shell {
            command: ShellCommand::Init,
        } => {
            ctx.emit(shell::INIT, json!({"script": shell::INIT}))?;
            Ok(0)
        }
        IntegrationCommand::Shell {
            command: ShellCommand::Recover { .. },
        } => unreachable!("shell recovery is handled before loading paths"),
    }
}
