//! The source's side of a move between joined machines: the package goes
//! to the destination's daemon over a duplex stream of the hub's links,
//! with the ticket the destination's claims of its spaces carry, and a
//! move whose arrival the destination could not report settles at the hub.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, bail, ensure};
use shoal_ids::{move_tickets::MoveTicket, space_ids::SpaceId};
use shoal_protocol::{
    hub::Member,
    protocol::{Operation, RemovalMethod},
    space_names::SpaceSelector,
};

use crate::{
    time::UnixTime,
    transfer::{ImportedMove, MachineDestination, MoveDestination, MoveRecord, MoveSettled},
};

impl Spaces {
    /// Deliver the package of the recorded move of the space `selector`
    /// names to the joined machine it goes to, and return how the import
    /// there went; an error leaves it unknown whether that machine claimed
    /// the space.
    pub async fn deliver_move(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<ImportedMove> {
        let space = self.space(selector).await?;
        let record = self.recorded_move(&space.id).await?;
        let destination = machine_destination(&record)?;
        let ticket = Some(&record.ticket);
        self.deliver_package(daemon, &space.id, destination, (ticket, false))
            .await
    }

    /// Deliver the package of a copy of the space `selector` names to the
    /// joined machine `destination` names, and return how the import there
    /// went.
    pub async fn deliver_copy(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        destination: &MachineDestination,
    ) -> Result<ImportedMove> {
        let space = self.space(selector).await?;
        self.deliver_package(daemon, &space.id, destination, (None, false))
            .await
    }

    /// Settle at the hub the recorded move of the space `selector` names,
    /// whose arrival the joined machine it goes to could not report: that
    /// machine claimed the space, or now no longer can. Fails while the
    /// hub cannot be asked.
    pub async fn settle_move(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<MoveSettled> {
        let space = self.space(selector).await?;
        let record = self.recorded_move(&space.id).await?;
        let destination = machine_destination(&record)?;
        let (machine, _) = daemon.named_machine(&destination.machine).await?;
        let settling = daemon.settle_move(&space.id, &machine.id, &record.ticket);
        Ok(match settling.await? {
            true => MoveSettled::Arrived,
            false => MoveSettled::Kept,
        })
    }

    /// The joined machine `named` names as a move's destination: another
    /// one than this, which is online, as a move needs both machines.
    pub async fn receiving_machine(&self, daemon: &dyn Daemon, named: &str) -> Result<Member> {
        let (machine, own) = daemon.named_machine(named).await?;
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
    pub(super) async fn deliver_package(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        destination: &MachineDestination,
        (ticket, removed): (Option<&MoveTicket>, bool),
    ) -> Result<ImportedMove> {
        let machine = self.receiving_machine(daemon, &destination.machine).await?;
        let file = self.staged_package(id);
        let receive = RemovalMethod::ReceiveMove {
            ticket: ticket.cloned(),
            resume: destination.resume,
            from: daemon.machine_name().await,
            removed,
        };
        let operation = Operation::Removal(receive);
        ImportedMove::try_from(
            daemon
                .deliver_file(&machine, operation, None, &file)
                .await?,
        )
    }

    /// The file the package of the space `id` waits in among those
    /// delivered from here.
    pub fn staged_package(&self, id: &SpaceId) -> std::path::PathBuf {
        (self.paths.move_packages()).join(crate::transfer::package_name(id))
    }

    /// Delete the package of the space `id` that waited among those
    /// delivered from here, once delivered or not; one left behind goes
    /// with the next move of the space.
    pub(super) fn discard_staged_package(&self, id: &SpaceId) {
        let _ = std::fs::remove_file(self.staged_package(id));
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
