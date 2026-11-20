//! `shoal internal import-moved`: the destination's side of a move between
//! joined machines. The daemon there runs it with the package on standard
//! input, and it continues the spaces as `shoal restore --from -` does,
//! their claims at the hub carrying the move's ticket, then prints how it
//! went for the daemon to answer the machine the spaces move from, where
//! they stay when nothing was recorded here.
use anyhow::Result;

use super::{
    super::reopen::{self, ReopenedExt},
    carried,
};
use crate::{
    cli::context::Context,
    protocol::{ErrorCode, RemoteError},
    refusal::Reason,
    transfer::{
        Arrived, CarriedRecord, ImportedMove, ImportedPackage, PackageImport, PackageSource,
    },
};
use shoal_ids::{move_tickets::MoveTicket, space_ids::SpaceId};

/// Continue the package on standard input, claiming its spaces with
/// `ticket`, which a copy lacks, queueing their agents' restores when
/// `restore`, and print how it went as the last line of standard output.
pub(in crate::cli::commands) async fn import_moved(
    ctx: &Context,
    ticket: Option<MoveTicket>,
    restore: bool,
) -> Result<i32> {
    let imported = match receive(ctx, ticket).await {
        Ok(imported) => match open(ctx, imported, restore).await {
            Ok((arrived, carried)) => ImportedMove::Arrived {
                arrived: Box::new(arrived),
                carried,
            },
            Err(error) => failed(&error),
        },
        Err(error) if nothing_imported(&error) => ImportedMove::Refused {
            error: format!("{error:#}"),
        },
        Err(error) => failed(&error),
    };
    outln!("{}", crate::client::output::json(&imported)?);
    Ok(0)
}

/// Have the daemon import the package on standard input, claiming its
/// spaces with `ticket`.
async fn receive(ctx: &Context, ticket: Option<MoveTicket>) -> Result<ImportedPackage> {
    let import = PackageImport {
        package: PackageSource::Stream,
        repository: None,
        sessions: None,
        ticket,
    };
    let mut input = tokio::io::stdin();
    crate::client::transfer::import_streamed(&ctx.paths, import, &mut input).await
}

/// Open the spaces `imported` recorded here, as `shoal restore --from`
/// does: its own space set up, with its post-setup hook run and, once that
/// succeeded, its agents' restores queued when `restore`, then the spaces
/// it carries, whose IDs it returns with where its own arrived.
async fn open(
    ctx: &Context,
    imported: ImportedPackage,
    restore: bool,
) -> Result<(Arrived, Vec<SpaceId>)> {
    let carried = (imported.carried.iter())
        .filter_map(|record| match record {
            CarriedRecord::Recorded { space, .. } => Some(space.clone()),
            CarriedRecord::Failed { .. } => None,
        })
        .collect();
    let mut reopened = reopen::prepare_set_up(ctx, imported.restored).await?;
    let post_setup = reopen::run_post_setup(ctx, &reopened).await;
    if post_setup.is_ok() && restore {
        reopened.queue_agents(ctx).await;
    }
    let failed = carried::open(ctx, imported.carried, restore).await;
    post_setup?;
    carried::ensure_opened(failed)?;
    let arrived = Arrived {
        space: reopened.space,
        queued: (reopened.queued).map_or_else(Vec::new, |queued| queued.queued),
    };
    Ok((arrived, carried))
}

/// That the import failed with `error` once the hub took its claim.
fn failed(error: &anyhow::Error) -> ImportedMove {
    ImportedMove::Failed {
        error: format!("{error:#}"),
    }
}

/// Whether `error` says the daemon recorded nothing of the package.
fn nothing_imported(error: &anyhow::Error) -> bool {
    let remote = error.downcast_ref::<RemoteError>();
    Reason::of(error) == Some(Reason::NotImported)
        || remote.is_some_and(|remote| remote.code == ErrorCode::NotImported)
}
