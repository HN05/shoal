use anyhow::Result;

use crate::cli::{
    args::lifecycle::{ExportArgs, ImportArgs, LifecycleCommand},
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
        LifecycleCommand::Export(ExportArgs {
            repository: Some(repository),
            output,
            confirmation,
            ..
        }) => transfer::export_all(&ctx, repository, output, confirmation.yes).await,
        LifecycleCommand::Export(ExportArgs {
            workspace,
            repository: None,
            output,
            confirmation,
            ..
        }) => transfer::export(&ctx, workspace, output, confirmation.yes).await,
        LifecycleCommand::Import(ImportArgs {
            files,
            repository,
            keep_sessions,
            new_sessions,
        }) => {
            let sessions = if keep_sessions {
                Some(true)
            } else if new_sessions {
                Some(false)
            } else {
                None
            };
            let options = transfer::ImportOptions {
                repository,
                sessions,
            };
            transfer::import(&ctx, files, options).await
        }
    }
}
