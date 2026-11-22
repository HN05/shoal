//! The source's side of a move between joined machines: the package goes
//! to the destination's daemon over a duplex stream of the hub's links,
//! with the ticket the destination's claims of its spaces carry, and a
//! move whose arrival the destination could not report settles at the hub.
//! A removed space's record moves the same way, and this machine forgets
//! it once the destination's claim stands.
use anyhow::{Result, bail, ensure};
use shoal_ids::{move_tickets::MoveTicket, space_ids::SpaceId};
use shoal_protocol::{
    hub::Member,
    protocol::{Operation, RemovalMethod},
    space_names::SpaceSelector,
};

use crate::{
    daemon::{
        hub::{Named, sending::deliver_file},
        manager::Manager,
    },
    time::UnixTime,
    transfer::{
        Carrying, ImportedMove, MachineDestination, MoveDestination, MoveRecord, MoveSettled,
        RecordArrival, RemovedDelivery,
    },
};

impl Manager {
    /// Deliver the package of the recorded move of the space `selector`
    /// names to the joined machine it goes to, and return how the import
    /// there went; an error leaves it unknown whether that machine claimed
    /// the space.
    pub(crate) async fn deliver_move(&self, selector: &SpaceSelector) -> Result<ImportedMove> {
        let space = self.space(selector).await?;
        let record = self.recorded_move(&space.id).await?;
        let destination = machine_destination(&record)?;
        let ticket = Some(&record.ticket);
        self.deliver_package(&space.id, destination, (ticket, false))
            .await
    }

    /// Deliver the package of a copy of the space `selector` names to the
    /// joined machine `destination` names, and return how the import there
    /// went.
    pub(crate) async fn deliver_copy(
        &self,
        selector: &SpaceSelector,
        destination: &MachineDestination,
    ) -> Result<ImportedMove> {
        let space = self.space(selector).await?;
        self.deliver_package(&space.id, destination, (None, false))
            .await
    }

    /// Deliver the removal record of the space `selector` names by ID, with
    /// those of the spaces its removal took that `carrying` carries, to the
    /// joined machine `destination` names, which records them as removed
    /// there, its claims carrying `ticket`, and forget here those it
    /// recorded; the records stay while its claim is unknown.
    pub(crate) async fn deliver_removed(
        &self,
        selector: &SpaceSelector,
        destination: &MachineDestination,
        carrying: &Carrying,
        ticket: &MoveTicket,
    ) -> Result<RemovedDelivery> {
        let exported = self.export_removed_moving(selector, carrying).await?;
        let id = exported.manifest.space.record.space_id.clone();
        let delivered = (self.deliver_package(&id, destination, (Some(ticket), true))).await;
        let _ = std::fs::remove_file(self.staged_package(&id));
        let machine = &destination.machine;
        let arrival = self.record_arrival(&id, machine, delivered).await?;
        Ok(RemovedDelivery { exported, arrival })
    }

    /// Settle at the hub the move `ticket` of the removal record of the
    /// space `selector` names to the joined machine named `machine`, whose
    /// arrival that machine could not report, and forget the record here
    /// once that machine claimed it. Fails while the hub cannot be asked.
    pub(crate) async fn settle_removed_move(
        &self,
        selector: &SpaceSelector,
        machine: &str,
        ticket: &MoveTicket,
    ) -> Result<MoveSettled> {
        let removed = self.removed_space(selector).await?;
        let Named { machine, .. } = self.hub.named_machine(self, machine).await?;
        let id = removed.space_id;
        if !(self.hub.settle_move(self, &id, &machine.id, ticket)).await? {
            return Ok(MoveSettled::Kept);
        }
        self.forget_moved_records(vec![id]).await?;
        Ok(MoveSettled::Arrived)
    }

    /// How the removal record of the space `id` arrived on the joined
    /// machine named `machine`, as `delivered` says, once this machine
    /// forgot the records that machine recorded.
    async fn record_arrival(
        &self,
        id: &SpaceId,
        machine: &str,
        delivered: Result<ImportedMove>,
    ) -> Result<RecordArrival> {
        Ok(match delivered {
            Ok(ImportedMove::Recorded { carried } | ImportedMove::Arrived { carried, .. }) => {
                let recorded = std::iter::once(id.clone()).chain(carried).collect();
                self.forget_moved_records(recorded).await?;
                RecordArrival::Recorded
            }
            Ok(ImportedMove::Refused { error }) => RecordArrival::Refused { error },
            Ok(ImportedMove::Failed { error }) => RecordArrival::Unknown {
                error: format!("recording it on machine {machine} failed: {error}"),
            },
            Err(error) => RecordArrival::Unknown {
                error: format!("machine {machine} did not say whether it took it: {error:#}"),
            },
        })
    }

    /// Settle at the hub the recorded move of the space `selector` names,
    /// whose arrival the joined machine it goes to could not report: that
    /// machine claimed the space, or now no longer can. Fails while the
    /// hub cannot be asked.
    pub(crate) async fn settle_move(&self, selector: &SpaceSelector) -> Result<MoveSettled> {
        let space = self.space(selector).await?;
        let record = self.recorded_move(&space.id).await?;
        let destination = machine_destination(&record)?;
        let Named { machine, .. } = self.hub.named_machine(self, &destination.machine).await?;
        let settling = (self.hub).settle_move(self, &space.id, &machine.id, &record.ticket);
        Ok(match settling.await? {
            true => MoveSettled::Arrived,
            false => MoveSettled::Kept,
        })
    }

    /// The joined machine `named` names as a move's destination: another
    /// one than this, which is online, as a move needs both machines.
    pub(crate) async fn receiving_machine(&self, named: &str) -> Result<Member> {
        let Named { machine, own } = self.hub.named_machine(self, named).await?;
        ensure!(
            !own,
            "the spaces run on machine {}; name another machine to move them to",
            machine.name
        );
        ensure!(
            machine.connected,
            "{}, and a move needs both machines online",
            machine.offline(UnixTime::now())
        );
        Ok(machine)
    }

    /// Deliver the package of the space `id`, waiting among the packages
    /// delivered from here, to the joined machine `destination` names,
    /// its spaces' claims there carrying `ticket`, recorded only as
    /// removed there when `removed`, and return how the import there went.
    async fn deliver_package(
        &self,
        id: &SpaceId,
        destination: &MachineDestination,
        (ticket, removed): (Option<&MoveTicket>, bool),
    ) -> Result<ImportedMove> {
        let machine = self.receiving_machine(&destination.machine).await?;
        let file = self.staged_package(id);
        let receive = RemovalMethod::ReceiveMove {
            ticket: ticket.cloned(),
            resume: destination.resume,
            from: self.hub.machine_name(self).await,
            removed,
        };
        let operation = Operation::Removal(receive);
        let (incoming, outgoing) = self
            .hub
            .open_duplex(self, &machine, operation, None)
            .await?;
        ImportedMove::try_from(deliver_file(&file, incoming, outgoing).await?)
    }

    /// The file the package of the space `id` waits in among those
    /// delivered from here.
    fn staged_package(&self, id: &SpaceId) -> std::path::PathBuf {
        (self.paths.move_packages()).join(crate::transfer::package_name(id))
    }
}

/// The joined machine the recorded move `record` goes to.
fn machine_destination(record: &MoveRecord) -> Result<&MachineDestination> {
    match &record.destination {
        MoveDestination::Machine(destination) => Ok(destination),
        _ => bail!(
            "the move of space {} goes to no joined machine",
            record.space
        ),
    }
}
