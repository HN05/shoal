//! Where `shoal move` takes spaces: the destination its flags name, or one
//! the user picks on a terminal, as the daemon that runs the move reads it.
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use super::{STANDARD_STREAM, targets};
use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use crate::transfer::{MachineDestination, MoveDestination, SshDestination};
use shoal_ids::space_ids::SpaceId;

/// Where a move takes spaces, as the user names it.
enum Named {
    /// Another machine's Shoal, through this SSH destination.
    Ssh(String),
    /// Another machine joined to the same hub, by its name or ID.
    Machine(String),
    /// A file, a directory of them or standard output, which
    /// `shoal restore --from` reads.
    File(PathBuf),
}

/// The destination a move goes to, with the temporary directory the
/// package that goes to standard output waits in.
pub(super) struct Chosen {
    pub(super) destination: MoveDestination,
    _stream: Option<tempfile::TempDir>,
}

/// What the destination picker offers.
#[derive(Clone, Copy)]
enum Kind {
    Ssh,
    File,
}

/// The spaces `args` names with the destination it names, or without one
/// the destination the user picks on a terminal, once the spaces and
/// repository it names exist and a checkpoint names one space, after
/// picking the spaces when it names none.
pub(super) async fn choose(ctx: &Context, args: &MoveArgs) -> Result<(Vec<SpaceId>, Chosen)> {
    let named = match (&args.ssh, &args.machine, &args.output) {
        (Some(host), _, _) => Some(Named::Ssh(host.clone())),
        (None, Some(machine), _) => Some(Named::Machine(machine.clone())),
        (None, None, Some(path)) => Some(Named::File(path.clone())),
        (None, None, None) => None,
    };
    if named.is_none() {
        ui::require_one_of(ctx, "destination", &["ssh", "machine", "output"])?;
    }
    if let Some(Named::File(path)) = &named {
        ensure_new_file(path)?;
    }
    // Only a file takes a removed space's record.
    let files = matches!(named, Some(Named::File(_)) | None);
    let spaces = targets::select(ctx, args, files).await?;
    ensure!(
        args.checkpoint.is_none() || spaces.len() == 1,
        "--checkpoint copies one workspace, whose checkpoints it numbers; name one"
    );
    let several = args.all || spaces.len() > 1;
    let named = match named {
        Some(named) => named,
        // Only a move over SSH takes them.
        None if args.no_resume || args.shoal.is_some() => ssh_host(ctx)?,
        None => pick(ctx, several).await?,
    };
    let chosen = match named {
        Named::Ssh(host) => Chosen {
            destination: MoveDestination::Ssh(SshDestination {
                host,
                shoal: args.shoal.clone(),
                resume: !args.no_resume,
            }),
            _stream: None,
        },
        Named::Machine(machine) => Chosen {
            destination: MoveDestination::Machine(MachineDestination {
                machine,
                resume: !args.no_resume,
            }),
            _stream: None,
        },
        Named::File(path) => file_destination(&path, several)?,
    };
    Ok((spaces, chosen))
}

/// Refuse a file `-o path` names that exists, before any picker opens; a
/// directory takes a file per space.
fn ensure_new_file(path: &Path) -> Result<()> {
    ensure!(
        path.as_os_str() == STANDARD_STREAM || path.is_dir() || !path.exists(),
        "{} exists; choose another file",
        path.display()
    );
    Ok(())
}

/// The destination `-o path` names for `several` spaces or one: standard
/// output for `-`, a directory that takes a file per space, or a file.
fn file_destination(path: &Path, several: bool) -> Result<Chosen> {
    if path.as_os_str() == STANDARD_STREAM {
        ensure!(
            !several,
            "several spaces go to a file each; name a directory with -o"
        );
        ensure!(
            !std::io::stdout().is_terminal(),
            "the move writes an archive, not text; name a file with -o, or pipe it, as in shoal move 7 -o - | ssh host shoal restore --from -"
        );
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("workspace.shoal");
        return Ok(Chosen {
            destination: MoveDestination::Client { path },
            _stream: Some(directory),
        });
    }
    // The daemon's worker runs elsewhere, so it takes an absolute path.
    let path = std::path::absolute(path).with_context(|| format!("find {}", path.display()))?;
    let destination = match several || path.is_dir() {
        true => MoveDestination::Directory { path },
        false => MoveDestination::File { path },
    };
    Ok(Chosen {
        destination,
        _stream: None,
    })
}

/// The destination the user picks: its kind, then the host or the path,
/// a directory for `several` spaces.
async fn pick(ctx: &Context, several: bool) -> Result<Named> {
    let kinds = [(Kind::Ssh, "SSH host"), (Kind::File, "File")];
    match ui::pick_choice(ctx, "Destination> ", &kinds)? {
        Kind::Ssh => ssh_host(ctx),
        Kind::File => file(ctx, several).await,
    }
}

/// The SSH host the user types.
fn ssh_host(ctx: &Context) -> Result<Named> {
    Ok(Named::Ssh(ui::input(ctx, "SSH host")?))
}

/// The file the user types, a directory for `several` spaces, never
/// standard output, which only `-o -` writes.
async fn file(ctx: &Context, several: bool) -> Result<Named> {
    let prompt = match several {
        true => "Directory",
        false => "File",
    };
    ui::input_until(ctx, prompt, None, |path| async move {
        ensure!(
            path != STANDARD_STREAM,
            "name a file; only -o - writes to standard output"
        );
        Ok(Named::File(PathBuf::from(path)))
    })
    .await
}
