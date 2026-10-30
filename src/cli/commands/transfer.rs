//! `shoal move`: moving workspaces to another Shoal, where `export` removes a
//! workspace and writes it to a file and `import` opens it again from one,
//! and moving repositories to another root directory.
mod export;
mod import;
mod root;

use anyhow::Result;

use crate::cli::{
    args::transfer::{ExportArgs, ImportArgs, MoveCommand},
    context::Context,
};

/// The file name that reads or writes the standard streams.
const STANDARD_STREAM: &str = "-";

pub(super) async fn run(ctx: &Context, command: MoveCommand) -> Result<i32> {
    match command {
        MoveCommand::Export(ExportArgs {
            repository: Some(repository),
            output,
            confirmation,
            ..
        }) => export::export_all(ctx, repository, output, confirmation.yes).await,
        MoveCommand::Export(ExportArgs {
            workspace,
            repository: None,
            output,
            confirmation,
            ..
        }) => export::export(ctx, workspace, output, confirmation.yes).await,
        MoveCommand::Import(ImportArgs {
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
            let options = import::ImportOptions {
                repository,
                sessions,
            };
            import::import(ctx, files, options).await
        }
        MoveCommand::Root {
            root,
            regular_dir,
            confirmation,
        } => {
            root::run(ctx, root, regular_dir, confirmation.yes).await?;
            Ok(0)
        }
    }
}
