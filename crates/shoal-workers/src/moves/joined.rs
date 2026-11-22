//! Delivering a move to another machine joined to the same hub: the daemon
//! sends the package there, and the destination's claim of the space at the
//! hub commits the move. When the destination's answer does not say that
//! nothing was recorded there, the move asks the hub whether it claimed the
//! space, and asks again while the hub cannot be asked, its agents stopped
//! meanwhile, since the space may continue there.
use std::time::Duration;

use anyhow::{Result, anyhow};

use super::{Steps, moving};
use crate::client::connection::CommandPaths;
use shoal_core::transfer::{Arrived, ImportedMove, MachineDestination, MoveSettled};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// How long a move whose arrival the hub cannot settle waits to ask again.
const ASK_AGAIN: Duration = Duration::from_secs(30);

/// Deliver the package of the recorded move of the space `id` to the
/// joined machine `destination` names: where it arrived, when the
/// destination said; an error once the space stays here, its move
/// forgotten and its agents restored here.
pub(super) async fn deliver(
    paths: &CommandPaths,
    id: &SpaceId,
    destination: &MachineDestination,
    steps: &impl Steps,
) -> Result<Option<Arrived>> {
    let machine = format!("machine {}", destination.machine);
    let error = match crate::client::transfer::deliver_move(paths, SpaceSelector::id(id)).await {
        Ok(ImportedMove::Arrived { arrived, .. }) => return Ok(Some(*arrived)),
        Ok(ImportedMove::Refused { error }) => {
            let refused = anyhow!("{machine} took none of it: {error}");
            return Err(moving::stays_here(paths, id, refused).await);
        }
        Ok(ImportedMove::Failed { error }) => anyhow!("its import on {machine} failed: {error}"),
        Ok(ImportedMove::Recorded { .. }) => {
            anyhow!("{machine} recorded it as removed without opening it")
        }
        Err(error) => error.context(format!("{machine} did not say whether it took the space")),
    };
    settle(paths, id, &machine, error, steps)
        .await
        .map(|()| None)
}

/// Settle the move of the space `id` to `machine`, whose arrival `error`
/// left unknown, at the hub: once the hub says that machine claimed the
/// space, it continues there and ends here; otherwise it stays here, its
/// move forgotten and its agents restored here. A move cancelled while the
/// hub cannot be asked stays recorded, for the daemon to finish.
pub(super) async fn settle(
    paths: &CommandPaths,
    id: &SpaceId,
    machine: &str,
    error: anyhow::Error,
    steps: &impl Steps,
) -> Result<()> {
    let ask = || crate::client::transfer::settle_move(paths, SpaceSelector::id(id));
    let meanwhile = "its agents stay stopped";
    match ask_hub(ask, &error, meanwhile, steps).await? {
        MoveSettled::Arrived => {
            super::warn(&format!(
                "warning: {error:#}; the hub says {machine} took the space, so it continues there"
            ));
            Ok(())
        }
        MoveSettled::Kept => Err(moving::stays_here(paths, id, error).await),
    }
}

/// How the hub, which `ask` asks, settled a move whose arrival `error` left
/// unknown, asking again every [`ASK_AGAIN`] while it cannot be asked,
/// saying `meanwhile` what stays as it is until it answers, until `steps`
/// says the move was cancelled.
pub(super) async fn ask_hub<Asked>(
    ask: impl Fn() -> Asked,
    error: &anyhow::Error,
    meanwhile: &str,
    steps: &impl Steps,
) -> Result<MoveSettled>
where
    Asked: Future<Output = Result<MoveSettled>>,
{
    loop {
        match ask().await {
            Ok(settled) => return Ok(settled),
            Err(asking) => super::warn(&format!(
                "warning: {error:#}, and the hub cannot say whether it did, so {meanwhile}; asking again in {} seconds: {asking:#}",
                ASK_AGAIN.as_secs()
            )),
        }
        steps.ensure_running()?;
        tokio::time::sleep(ASK_AGAIN).await;
    }
}
