//! `shoal move`: moving spaces to another Shoal, where `export` ends a space
//! here, or copies one, and writes it to a file, which `shoal restore
//! --from` continues, and `ssh` pipes one into the other.
mod export;
mod ssh;

use std::{io::IsTerminal, path::PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::cli::{
    args::transfer::{ExportArgs, MoveCommand},
    context::Context,
};

/// The file name that reads or writes the standard streams.
pub(super) const STANDARD_STREAM: &str = "-";

pub(super) async fn run(ctx: &Context, command: MoveCommand) -> Result<i32> {
    match command {
        MoveCommand::Export(ExportArgs {
            repository: Some(repository),
            output,
            keep,
            no_children,
            confirmation,
            ..
        }) => {
            let directory = output.context("--all writes a file each; name a directory with -o")?;
            let spaces = export::repository_spaces(ctx, repository).await?;
            let carrying = export::carrying(no_children, &spaces);
            match keep {
                true => export::copy::copy_into(ctx, &spaces, directory, None, &carrying).await,
                false => {
                    let yes = confirmation.yes;
                    export::export_into(ctx, &spaces, directory, yes, &carrying).await
                }
            }
        }
        MoveCommand::Export(ExportArgs {
            space,
            repository: None,
            output,
            keep,
            checkpoint,
            no_children,
            confirmation,
            ..
        }) => {
            let file = output.map_or_else(standard_output, Ok)?;
            match keep {
                true => export::copy::export_copy(ctx, space, file, checkpoint, no_children).await,
                false => export::export(ctx, space, file, confirmation.yes, no_children).await,
            }
        }
        MoveCommand::Ssh(args) => ssh::run(ctx, args).await,
    }
}

/// Standard output for an export without -o, refusing a terminal, which
/// cannot show the archive.
fn standard_output() -> Result<PathBuf> {
    ensure!(
        !std::io::stdout().is_terminal(),
        "the export is an archive, not text; name a file with -o, or pipe it, as in shoal move export | ssh host shoal restore --from -"
    );
    Ok(PathBuf::from(STANDARD_STREAM))
}
