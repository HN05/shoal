use anyhow::Result;

use crate::cli::{
    args::lifecycle::LifecycleCommand,
    commands::{cleanup, stop, transfer, turns, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: LifecycleCommand) -> Result<i32> {
    match command {
        LifecycleCommand::Stop {
            targets,
            interrupt: true,
            ..
        } => turns::interrupt(&ctx, targets.workspace).await,
        LifecycleCommand::Stop {
            targets,
            yes,
            skip_children,
            interrupt: false,
        } => stop::run(&ctx, targets, yes, skip_children).await,
        LifecycleCommand::Rm {
            workspace,
            confirmation,
        } => workspaces::remove(&ctx, workspace, confirmation.yes).await,
        LifecycleCommand::Cleanup { kind, args } => cleanup::run(&ctx, kind, args).await,
        LifecycleCommand::Move { command } => transfer::run(&ctx, command).await,
    }
}
