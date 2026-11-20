//! Where `shoal move` takes spaces: the destination its flags name, or one
//! the user picks on a terminal.
use std::path::PathBuf;

use anyhow::{Result, ensure};

use super::{STANDARD_STREAM, export};
use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use shoal_protocol::space_names::SpaceSelector;

/// Where a move takes spaces.
pub(super) enum Destination {
    /// Another machine's Shoal, through this SSH destination.
    Ssh(String),
    /// A file, a directory of them or standard output, which
    /// `shoal restore --from` reads.
    File(PathBuf),
}

impl Destination {
    /// The destination the flags of `args` name.
    fn named(args: &MoveArgs) -> Option<Self> {
        match (&args.ssh, &args.output) {
            (Some(host), _) => Some(Self::Ssh(host.clone())),
            (None, Some(path)) => Some(Self::File(path.clone())),
            (None, None) => None,
        }
    }

    /// Whether the spaces below a moved space go with it unless the move
    /// says otherwise.
    pub(super) fn recursive(&self) -> bool {
        match self {
            Self::Ssh(_) | Self::File(_) => true,
        }
    }
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
pub(super) async fn choose(
    ctx: &Context,
    args: &MoveArgs,
) -> Result<(Vec<SpaceSelector>, Destination)> {
    let names = args.space.clone();
    if let Some(destination) = Destination::named(args) {
        return Ok((names, destination));
    }
    ui::require_one_of(ctx, "destination", &["ssh", "output"])?;
    export::ensure_targets(ctx, &names, args.repository.as_deref()).await?;
    let all = args.all;
    let names = match all || !names.is_empty() {
        true => names,
        false => ui::select_spaces(ctx, names, true).await?,
    };
    export::copy::ensure_one_at_checkpoint(names.len(), args.checkpoint)?;
    let several = all || names.len() > 1;
    let destination = match args.no_resume || args.shoal.is_some() {
        // Only a move over SSH takes them.
        true => ssh_host(ctx)?,
        false => pick(ctx, several).await?,
    };
    Ok((names, destination))
}

/// The destination the user picks: its kind, then the host or the path,
/// a directory for `several` spaces.
async fn pick(ctx: &Context, several: bool) -> Result<Destination> {
    let kinds = [(Kind::Ssh, "SSH host"), (Kind::File, "File")];
    match ui::pick_choice(ctx, "Destination> ", &kinds)? {
        Kind::Ssh => ssh_host(ctx),
        Kind::File => file(ctx, several).await,
    }
}

/// The SSH host the user types.
fn ssh_host(ctx: &Context) -> Result<Destination> {
    Ok(Destination::Ssh(ui::input(ctx, "SSH host")?))
}

/// The file the user types, a directory for `several` spaces, never
/// standard output, which only `-o -` writes.
async fn file(ctx: &Context, several: bool) -> Result<Destination> {
    let prompt = match several {
        true => "Directory",
        false => "File",
    };
    ui::input_until(ctx, prompt, None, |path| async move {
        ensure!(
            path != STANDARD_STREAM,
            "name a file; only -o - writes to standard output"
        );
        Ok(Destination::File(PathBuf::from(path)))
    })
    .await
}
