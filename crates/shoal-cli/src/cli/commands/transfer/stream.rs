//! A move to standard output, which needs this command until it copied the
//! package: refused where ending the spaces would end the command first,
//! and holding the move's lock until the command copied the package and
//! ended the space here, so a command that goes before leaves the space to
//! the daemon to finish.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::cli::context::Context;
use crate::transfer::{MoveDestination, MoveLock, MoveOrder, MoveReport, SpaceMoved};
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::moves::{self, Planned};

/// A move to standard output in progress, holding the move's lock, with
/// the file its package waits in.
pub(super) struct Streaming {
    lock: MoveLock,
    package: PathBuf,
}

impl Streaming {
    /// The move `order` to standard output, once nothing it ends would end
    /// this command first, under the move's lock; none for another
    /// destination.
    pub(super) async fn start(ctx: &Context, order: &MoveOrder) -> Result<Option<Self>> {
        let MoveDestination::Client { path } = &order.destination else {
            return Ok(None);
        };
        ensure_survives(ctx, order).await?;
        let space = order.spaces.first().context("a move takes a space")?;
        let lock = MoveLock::take(&ctx.paths, space).await?;
        let package = path.clone();
        Ok(Some(Self { lock, package }))
    }

    /// Copy the package of the move `order` that `report` describes to
    /// standard output, then end its space here, which its reader has.
    pub(super) async fn deliver(
        self,
        ctx: &Context,
        order: &MoveOrder,
        report: MoveReport,
    ) -> Result<i32> {
        let [moved] = report.moved.as_slice() else {
            let failed = report.failed.first().map(|failed| failed.error.as_str());
            anyhow::bail!("{}", failed.unwrap_or("the move wrote no package"));
        };
        copy_to_stdout(&self.package)?;
        if !order.keep {
            end_here(ctx, moved).await?;
        }

        self.lock.release();
        eprintln!("Exported {}", exported(order, moved));
        Ok(0)
    }
}

/// Copy the package in `path` to standard output.
fn copy_to_stdout(path: &Path) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    crate::stdout::write(|stdout| {
        std::io::copy(&mut file, stdout)?;
        stdout.flush()
    })?;
    Ok(())
}

/// End the `moved` space here, whose package its reader took, unless it
/// was already removed, which its record moved.
async fn end_here(ctx: &Context, moved: &SpaceMoved) -> Result<()> {
    let selector = SpaceSelector::id(&moved.id);
    if crate::client::workspaces::present_space(&ctx.paths, &selector)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let ending = crate::client::transfer::end_moved(&ctx.paths, selector);
    ending.await.with_context(|| {
        format!(
            "it now exists here and wherever its package went; once the work you want is there, shoal rm {} removes it here",
            moved.id
        )
    })?;
    Ok(())
}

/// What a move wrote of the `moved` space: it, or with `--keep` a copy.
fn exported(order: &MoveOrder, moved: &SpaceMoved) -> String {
    match order.keep {
        true => format!("a copy of {}", moved.name),
        false => moved.name.to_string(),
    }
}

/// Refuse the move `order` to standard output when it would end the chat
/// this process runs in, or a space this terminal closes with, before the
/// package reached standard output.
async fn ensure_survives(ctx: &Context, order: &MoveOrder) -> Result<()> {
    for planned in moves::plan(&ctx.paths, order).await? {
        let Planned::Ending { checked, .. } = planned else {
            continue;
        };
        let carried = (checked.carried.iter()).map(|space| (&space.id, space.name.as_str()));
        for (id, name) in std::iter::once((checked.id(), checked.name().as_str())).chain(carried) {
            ensure!(
                crate::env::own_space().as_ref() != Some(id),
                "this process runs in {name}, which the move ends before the package reaches standard output; move it to a file with -o, or from another terminal"
            );
            ensure!(
                !crate::cli::terminal::closes_with(&ctx.paths, id).await?,
                "this terminal closes when the move ends {name}, ending the stream; move it to a file with -o, or from another terminal"
            );
        }
    }
    Ok(())
}
