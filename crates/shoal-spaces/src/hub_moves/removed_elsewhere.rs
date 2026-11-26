//! Finding the removal record of a space another joined machine removed:
//! the hub's directory lists the machine that keeps it, which answers for
//! it as it answers its own callers.
use anyhow::{Context as _, Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::Member,
    model::{LocatedRemoved, RemovedSpace},
    protocol::{Body, Operation, RemovalMethod},
    space_names::SpaceSelector,
};

use crate::{Spaces, daemon::Daemon, scope::Caller};

impl Spaces {
    /// Kept here, where `caller` may restore it, or on the other joined
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

impl Spaces {
    /// As [`Self::restorable_space`] finds it. Refused, naming the other
    /// joined machine that keeps its record, when this one keeps none.
    pub async fn restorable_here(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<RemovedSpace> {
        let missing = match self.restorable_space(daemon, selector).await {
            Ok(space) => return Ok(space),
            Err(missing) => missing,
        };
        if let Some(id) = selector.given_id() {
            self.refuse_kept_elsewhere(daemon, id).await?;
        }
        Err(missing)
    }

    /// Names that machine and how to open the space here.
    pub(crate) async fn refuse_kept_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<()> {
        if self.removed_here(id).await? {
            return Ok(());
        }
        let selector = SpaceSelector::id(id);
        if let Ok(Some(machine)) = daemon.record_keeper(&selector).await {
            bail!(
                "machine {} keeps the record of removed space {id}; restore it there, or open it here with shoal move {id} -o FILE, then shoal restore --from FILE",
                machine.name
            );
        }
        Ok(())
    }
}

/// As `machine` answers `caller`.
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
