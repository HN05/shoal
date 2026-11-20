//! `shoal move`: moving spaces to another Shoal, over SSH or through a file
//! that `shoal restore --from` continues.
mod destination;
mod export;
mod ssh;

use std::{io::IsTerminal, path::PathBuf};

use anyhow::{Result, ensure};

use crate::cli::{args::transfer::MoveArgs, context::Context};
use destination::Destination;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

/// The file name that reads or writes the standard streams.
pub(super) const STANDARD_STREAM: &str = "-";

/// What a move takes, whatever its destination.
struct Moving {
    /// The spaces named or picked; none with `repository`.
    names: Vec<SpaceSelector>,
    /// The repository whose every space moves.
    repository: Option<String>,
    /// Whether copies move, leaving the spaces here.
    keep: bool,
    /// The earlier checkpoint a copied workspace goes at.
    checkpoint: Option<CheckpointNumber>,
    /// Whether the spaces below each space go with it.
    recursive: bool,
    /// Whether ending the spaces needs no confirmation.
    yes: bool,
}

impl Moving {
    /// What `args` moves, the spaces `names` select, to `destination`.
    fn new(args: &MoveArgs, names: Vec<SpaceSelector>, destination: &Destination) -> Self {
        Self {
            names,
            repository: args.repository.clone(),
            keep: args.keep,
            checkpoint: args.checkpoint,
            recursive: args.recursive(destination.recursive()),
            yes: args.confirmation.yes,
        }
    }
}

/// `shoal move`: move the spaces `args` selects to the destination it
/// names, or to the one the user picks.
pub(super) async fn run(ctx: &Context, args: MoveArgs) -> Result<i32> {
    let (names, destination) = destination::choose(ctx, &args).await?;
    let moving = Moving::new(&args, names, &destination);
    match destination {
        Destination::Ssh(host) => {
            let remote = ssh::Remote::new(host, args.shoal, !args.no_resume);
            ssh::run(ctx, remote, moving).await
        }
        Destination::File(path) => {
            ensure!(
                !args.no_resume && args.shoal.is_none(),
                "--no-resume and --shoal apply only to a move over SSH"
            );
            to_file(ctx, path, moving).await
        }
    }
}

/// Write the spaces `moving` takes to `path`: a file, a directory of a
/// file per space, or standard output.
async fn to_file(ctx: &Context, path: PathBuf, moving: Moving) -> Result<i32> {
    if let Some(repository) = moving.repository {
        ensure!(
            path.as_os_str() != STANDARD_STREAM,
            "--all writes a file each; name a directory with -o"
        );
        let spaces = export::repository_spaces(ctx, repository).await?;
        let carrying = export::carrying(moving.recursive, &spaces);
        return match moving.keep {
            true => export::copy::copy_into(ctx, &spaces, path, None, &carrying).await,
            false => export::export_into(ctx, &spaces, path, moving.yes, &carrying).await,
        };
    }
    ensure_stream_piped(&path)?;
    match moving.keep {
        true => {
            let (names, checkpoint) = (moving.names, moving.checkpoint);
            export::copy::export_copy(ctx, names, path, checkpoint, moving.recursive).await
        }
        false => export::export(ctx, moving.names, path, moving.yes, moving.recursive).await,
    }
}

/// Refuse to write standard output for `-` when it is a terminal, which
/// cannot show the archive.
fn ensure_stream_piped(path: &std::path::Path) -> Result<()> {
    ensure!(
        path.as_os_str() != STANDARD_STREAM || !std::io::stdout().is_terminal(),
        "the move writes an archive, not text; name a file with -o, or pipe it, as in shoal move 7 -o - | ssh host shoal restore --from -"
    );
    Ok(())
}
