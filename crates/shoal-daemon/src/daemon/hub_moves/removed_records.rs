//! Moving a removed space's record to another joined machine: the record,
//! with those of the spaces its removal took that the move carries, goes
//! to the destination's daemon as a move's package does, which records them
//! as removed there, claiming each with the move's ticket, and this machine
//! forgets each record that machine claimed. A record whose claim the
//! destination could not report is settled at the hub, and one it did not
//! claim stays here.
use anyhow::{Result, bail};
use shoal_ids::{move_tickets::MoveTicket, space_ids::SpaceId};
use shoal_protocol::{model::RemovedSpace, space_names::SpaceSelector};

use crate::{
    daemon::{hub::Named, manager::Manager},
    transfer::{
        Carrying, ImportedMove, MachineDestination, Manifest, MoveSettled, RecordArrival,
        RemovedDelivery,
    },
};

impl Manager {
    /// Deliver the removal record of the space `selector` names by ID, with
    /// those of the spaces its removal took that `carrying` carries, to the
    /// joined machine `destination` names, which records them as removed
    /// there, its claims carrying `ticket`, and forget here those it
    /// claimed; the records stay while its claim is unknown.
    pub(crate) async fn deliver_removed(
        &self,
        selector: &SpaceSelector,
        destination: &MachineDestination,
        carrying: &Carrying,
        ticket: &MoveTicket,
    ) -> Result<RemovedDelivery> {
        let id = removed_id(selector)?;
        let mut exported = self.export_removed_moving(selector, carrying).await?;
        let delivered = (self.deliver_package(&id, destination, (Some(ticket), true))).await;
        self.discard_staged_package(&id);
        let machine = (destination.machine.as_str(), ticket);
        let (arrival, warnings) = self
            .record_arrival(&exported.manifest, machine, delivered)
            .await?;
        exported.warnings.extend(warnings);
        Ok(RemovedDelivery { exported, arrival })
    }

    /// Settle at the hub the move `ticket` of the removal record of the
    /// space `selector` names, with the records of the spaces `carried`, to
    /// the joined machine named `machine`, whose arrival that machine could
    /// not report, and forget here each record that machine claimed; how
    /// the hub settled the space's own. Fails while the hub cannot be
    /// asked, forgetting none.
    pub(crate) async fn settle_removed_move(
        &self,
        selector: &SpaceSelector,
        carried: &[SpaceId],
        machine: &str,
        ticket: &MoveTicket,
    ) -> Result<MoveSettled> {
        let id = removed_id(selector)?;
        self.removed_space(selector).await?;
        let Named { machine, .. } = self.hub.named_machine(self, machine).await?;
        let mut claimed = Vec::new();
        for record in std::iter::once(&id).chain(carried) {
            if (self.hub.settle_move(self, record, &machine.id, ticket)).await? {
                claimed.push(record.clone());
            }
        }
        // The destination claims the space's own record before any other.
        if !claimed.contains(&id) {
            return Ok(MoveSettled::Kept);
        }
        self.forget_moved_records(claimed).await?;
        Ok(MoveSettled::Arrived)
    }

    /// How the removal records the package `manifest` holds arrived on the
    /// joined machine named `machine`, to which they moved with `ticket`,
    /// as `delivered` says, once this machine forgot those that machine
    /// claimed, with a warning for each carried record that stays here.
    async fn record_arrival(
        &self,
        manifest: &Manifest,
        (machine, ticket): (&str, &MoveTicket),
        delivered: Result<ImportedMove>,
    ) -> Result<(RecordArrival, Vec<String>)> {
        let arrival = match delivered {
            Ok(ImportedMove::Recorded { carried } | ImportedMove::Arrived { carried, .. }) => {
                let moved = (manifest, carried.as_slice());
                let warnings = self.forget_arrived(moved, (machine, ticket)).await?;
                return Ok((RecordArrival::Recorded, warnings));
            }
            Ok(ImportedMove::Refused { error }) => RecordArrival::Refused { error },
            Ok(ImportedMove::Failed { error }) => RecordArrival::Unknown {
                error: format!("recording it on machine {machine} failed: {error}"),
            },
            Err(error) => RecordArrival::Unknown {
                error: format!("machine {machine} did not say whether it took it: {error:#}"),
            },
        };
        Ok((arrival, Vec::new()))
    }

    /// Forget the records of the package `manifest` that the joined machine
    /// named `machine` recorded, its own and those it lists in `recorded`,
    /// and each other carried record the hub says it claimed with `ticket`,
    /// as when the answer to its claim never came there; a warning for each
    /// that stays here.
    async fn forget_arrived(
        &self,
        (manifest, recorded): (&Manifest, &[SpaceId]),
        (machine, ticket): (&str, &MoveTicket),
    ) -> Result<Vec<String>> {
        let mut forgotten = vec![manifest.space.record.space_id.clone()];
        let mut warnings = Vec::new();
        let unlisted =
            carried_records(manifest).filter(|record| !recorded.contains(&record.space_id));
        for record in unlisted {
            match self.claimed_at_hub(&record.space_id, machine, ticket).await {
                Ok(true) => forgotten.push(record.space_id.clone()),
                Ok(false) => warnings.push(format!(
                    "the record of {} stays here, since machine {machine} recorded none of it",
                    record.name
                )),
                Err(error) => warnings.push(format!(
                    "the record of {} stays here, and the hub cannot say whether machine {machine} has it too: {error:#}",
                    record.name
                )),
            }
        }
        forgotten.extend(recorded.iter().cloned());
        self.forget_moved_records(forgotten).await?;
        Ok(warnings)
    }

    /// Whether the joined machine named `machine` claimed the space `id`
    /// with `ticket`, as the hub settles it, which refuses that claim from
    /// then on when it did not.
    async fn claimed_at_hub(
        &self,
        id: &SpaceId,
        machine: &str,
        ticket: &MoveTicket,
    ) -> Result<bool> {
        let Named { machine, .. } = self.hub.named_machine(self, machine).await?;
        self.hub.settle_move(self, id, &machine.id, ticket).await
    }
}

/// The ID of the removed space `selector` names, which only its ID can.
fn removed_id(selector: &SpaceSelector) -> Result<SpaceId> {
    match selector.given_id() {
        Some(id) => Ok(id.clone()),
        None => bail!(
            "no restorable space {selector}; a removed space has no number, so select it by its ID"
        ),
    }
}

/// The removal records the package `manifest` carries below its own.
fn carried_records(manifest: &Manifest) -> impl Iterator<Item = &RemovedSpace> {
    manifest.carried.iter().map(|carried| &carried.space.record)
}
