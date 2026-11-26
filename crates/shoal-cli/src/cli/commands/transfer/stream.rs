//! A move to standard output, which needs this command until it copied the
//! package. It holds the move's lock until then, so a command that goes before
//! leaves the space to the daemon to finish.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::cli::context::Context;
use crate::transfer::{MoveDestination, MoveLock, MoveOrder, MoveReport, SpaceMoved};
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::moves::{self, Planned};

pub(super) struct Streaming {
    lock: MoveLock,
    package: PathBuf,
}

impl Streaming {
    /// Refused where ending the spaces would end this command first.
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
            end_moved_space(ctx, moved).await?;
        }

        self.lock.release();
        eprintln!("Exported {}", exported(order, moved));
        Ok(0)
    }
}

fn copy_to_stdout(path: &Path) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    crate::stdout::write(|stdout| {
        std::io::copy(&mut file, stdout)?;
        stdout.flush()
    })?;
    Ok(())
}

/// A removed space's record stays where it is kept.
async fn end_moved_space(ctx: &Context, moved: &SpaceMoved) -> Result<()> {
    if moves::takes_record(&ctx.paths, &moved.id).await {
        return Ok(());
    }
    let selector = SpaceSelector::id(&moved.id);
    let ending = crate::client::transfer::end_moved(&ctx.paths, selector);
    ending.await.with_context(|| {
        format!(
            "it now exists where it runs and wherever its package went; once the work you want is there, shoal rm {} removes it there",
            moved.id
        )
    })?;
    Ok(())
}

fn exported(order: &MoveOrder, moved: &SpaceMoved) -> String {
    match order.keep {
        true => format!("a copy of {}", moved.name),
        false => moved.name.to_string(),
    }
}

async fn ensure_survives(ctx: &Context, order: &MoveOrder) -> Result<()> {
    // Another machine's space runs neither this process nor its terminal.
    if moves::runs_elsewhere(&ctx.paths, order).await? {
        return Ok(());
    }
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
                !shoal_workers::hosting::closes_with(&ctx.paths, id).await?,
                "this terminal closes when the move ends {name}, ending the stream; move it to a file with -o, or from another terminal"
            );
        }
    }
    Ok(())
}
