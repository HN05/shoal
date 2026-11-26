//! `shoal internal import-moved`: the destination's side of a move between
//! joined machines.
use anyhow::Result;

use shoal_core::{
    refusal::Reason,
    transfer::{
        Arrived, CarriedRecord, Claim, ImportedMove, ImportedPackage, PackageImport, PackageSource,
    },
};

use crate::{
    client::connection::CommandPaths,
    protocol::{ErrorCode, RemoteError},
    reopening::{self, Reopening},
};
use shoal_ids::space_ids::SpaceId;

/// Continue the package on standard input, claiming its spaces as `claim`
/// says, queueing their agents' restores when `restore`, and print how it
/// went as the last line of standard output.
pub async fn import_moved(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    claim: Claim,
    restore: bool,
) -> Result<i32> {
    let imported = match receive(paths, claim).await {
        Ok(imported) => match open(paths, reopening, imported, restore).await {
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
    shoal_core::outln!("{}", crate::client::output::json(&imported)?);
    Ok(0)
}

/// Have the daemon import the package on standard input, claiming its
/// spaces as `claim` says.
async fn receive(paths: &CommandPaths, claim: Claim) -> Result<ImportedPackage> {
    let import = PackageImport {
        package: PackageSource::Stream,
        repository: None,
        sessions: None,
        claim,
    };
    let mut input = tokio::io::stdin();
    crate::client::transfer::import_streamed(paths, import, &mut input).await
}

/// Open the spaces `imported` recorded here, as `shoal restore --from`
/// does: its own space set up, with its post-setup hook run and, once that
/// succeeded, its agents' restores queued when `restore`, then the spaces
/// it carries, whose IDs it returns with where its own arrived.
async fn open(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    imported: ImportedPackage,
    restore: bool,
) -> Result<(Arrived, Vec<SpaceId>)> {
    let carried = (imported.carried.iter())
        .filter_map(|record| match record {
            CarriedRecord::Recorded { space, .. } => Some(space.clone()),
            CarriedRecord::Failed { .. } => None,
        })
        .collect();
    let mut reopened = reopening::prepare_set_up(paths, reopening, imported.restored).await?;
    let post_setup = reopening::run_post_setup(paths, reopening, &reopened).await;
    if post_setup.is_ok() && restore {
        reopening::queue_agents(paths, reopening, &mut reopened).await;
    }
    let failed = reopening::open_carried(paths, reopening, imported.carried, restore).await;
    post_setup?;
    reopening::ensure_carried_opened(failed)?;
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
