//! Finding the removal record of a space another joined machine removed:
//! the hub's directory lists the machine that keeps it, which answers for
//! it as it answers its own callers, so a move to a file can take it from
//! there.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::Member,
    model::{LocatedRemoved, RemovedSpace},
    protocol::{Body, Operation, RemovalMethod},
    space_names::SpaceSelector,
};

use crate::{Spaces, daemon::Daemon, scope::Caller};

impl Spaces {
    /// The removed space `selector` names by ID, with where its record is
    /// kept: here, where `caller` may restore it, or on the other joined
    /// machine the hub lists as keeping it, which checks that for `caller`.
    pub async fn locate_removed(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<LocatedRemoved> {
        let missing = match self.restorable_space(daemon, selector).await {
            Ok(space) => {
                if let Some(caller) = caller {
                    self.ensure_restorable_by(daemon, selector, caller).await?;
                }
                return Ok(LocatedRemoved::Here(Box::new(space)));
            }
            Err(missing) => missing,
        };
        let Ok(Some(machine)) = daemon.record_keeper(selector).await else {
            return Err(missing);
        };
        let space = record_kept_on(daemon, &machine, selector, caller).await?;
        Ok(LocatedRemoved::Elsewhere {
            space: Box::new(space),
            machine: Box::new(machine),
        })
    }
}

/// The removal record of the space `selector` names that `machine` keeps,
/// as it answers `caller`.
async fn record_kept_on(
    daemon: &dyn Daemon,
    machine: &Member,
    selector: &SpaceSelector,
    caller: Option<&Caller>,
) -> Result<RemovedSpace> {
    let space = selector.clone();
    let asking = Operation::Removal(RemovalMethod::RemovedSpace { space });
    match daemon.ask_machine(machine, asking, caller).await? {
        Body::RemovedSpaces(spaces) => (spaces.into_iter().next()).with_context(|| {
            format!(
                "machine {} keeps no record of space {selector}",
                machine.name
            )
        }),
        body => Err(body.unexpected("RemovedSpaces")),
    }
}
