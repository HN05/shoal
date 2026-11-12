use anyhow::Result;

use crate::{
    cli::{
        args::lifecycle::{LifecycleCommand, StopArgs},
        commands::{cleanup, stop, transfer, turns, workspaces},
        context::Context,
    },
    client::processes::Selection,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: LifecycleCommand) -> Result<i32> {
    match command {
        LifecycleCommand::Stop(StopArgs {
            targets,
            interrupt: true,
            ..
        }) => turns::interrupt(&ctx, targets.space).await,
        LifecycleCommand::Stop(StopArgs {
            targets,
            processes: true,
            ..
        }) => stop::processes(&ctx, targets.space, Selection::Processes).await,
        LifecycleCommand::Stop(StopArgs {
            targets,
            leftovers: true,
            ..
        }) => stop::processes(&ctx, targets.space, Selection::Leftovers).await,
        LifecycleCommand::Stop(args) => stop::run(&ctx, args).await,
        LifecycleCommand::Rm(args) => workspaces::remove(&ctx, args).await,
        LifecycleCommand::Cleanup { kind, args } => cleanup::run(&ctx, kind, args).await,
        LifecycleCommand::Move { command } => transfer::run(&ctx, command).await,
    }
}
