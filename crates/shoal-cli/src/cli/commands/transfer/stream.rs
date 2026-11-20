//! A move to standard output, which needs this command until it copied the
//! package: refused where ending the spaces would end the command first.
use anyhow::{Result, ensure};

use crate::cli::context::Context;
use crate::transfer::MoveOrder;
use shoal_workers::moves::{self, Planned};

/// Refuse the move `order` to standard output when it would end the chat
/// this process runs in, or a space this terminal closes with, before the
/// package reached standard output.
pub(super) async fn ensure_survives(ctx: &Context, order: &MoveOrder) -> Result<()> {
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
