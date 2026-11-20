//! The source's side of a move between joined machines: the package goes
//! to the destination's daemon over a duplex stream of the hub's links,
//! with the ticket the destination's claims of its spaces carry, and a
//! move whose arrival the destination could not report settles at the hub.
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
    transfer::{ImportedMove, MachineDestination, MoveDestination, MoveRecord, MoveSettled},
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
        self.deliver_package(&space.id, destination, ticket).await
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
        self.deliver_package(&space.id, destination, None).await
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
    /// its spaces' claims there carrying `ticket`, and return how the
    /// import there went.
    async fn deliver_package(
        &self,
        id: &SpaceId,
        destination: &MachineDestination,
        ticket: Option<&MoveTicket>,
    ) -> Result<ImportedMove> {
        let machine = self.receiving_machine(&destination.machine).await?;
        let file = (self.paths.move_packages()).join(crate::transfer::package_name(id));
        let receive = RemovalMethod::ReceiveMove {
            ticket: ticket.cloned(),
            resume: destination.resume,
            from: self.hub.machine_name(self).await,
        };
        let operation = Operation::Removal(receive);
        let (incoming, outgoing) = self
            .hub
            .open_duplex(self, &machine, operation, None)
            .await?;
        ImportedMove::try_from(deliver_file(&file, incoming, outgoing).await?)
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
