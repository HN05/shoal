//! The hub's directory of every joined machine's spaces: the reports each
//! machine sends of its own, the view the hub serves from them, the
//! machine that owns each space ID, and the one sequence of their numbers.
use anyhow::Result;
use shoal_daemon_base::notifications::About;
use shoal_ids::{
    member_ids::MemberId,
    move_tickets::MoveTicket,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::hub::{
    Member, MemberKind,
    directory::{Conflict, Directory, RegisteredRepository, Report},
    numbering::Numbering,
};

use crate::{Hub, daemon::Daemon, packages::takeovers};

mod fences;
mod numbers;
mod records;
mod repositories;

pub(super) use records::{forget, owner};

impl Hub {
    /// Record the machine `member`'s `report` of its spaces, the numbers
    /// it holds without a space, `reserved`, and its `repositories`, none
    /// when they did not change, alerting the user to each space ID it
    /// reported that another machine owns, deleting the packages of the
    /// spaces that ended there, and return the numbers the hub keeps for
    /// the machine.
    pub(super) async fn record_report(
        &self,
        daemon: &dyn Daemon,
        member: &MemberId,
        report: Report,
        (reserved, repositories): (Vec<SpaceNumber>, Option<Vec<RegisteredRepository>>),
    ) -> Result<Numbering> {
        let member = member.clone();
        let (conflicts, numbering) = (self.store)
            .write(move |tx| {
                numbers::reserve_held(tx, &member, &reserved)?;
                let conflicts = records::apply(tx, &member, report)?;
                if let Some(repositories) = &repositories {
                    repositories::replace(tx, &member, repositories)?;
                }
                numbers::replace_held(tx, &member)?;
                Ok((conflicts, numbers::kept_for(tx, &member)?))
            })
            .await?;
        // A space that ended, or whose ID another machine took over, takes
        // its package with it.
        self.delete_ended_packages().await;
        self.alert_conflicts(daemon, conflicts).await?;
        Ok(numbering)
    }

    /// Alert the user to each of `conflicts`, which a report raised.
    async fn alert_conflicts(&self, daemon: &dyn Daemon, conflicts: Vec<Conflict>) -> Result<()> {
        if conflicts.is_empty() {
            return Ok(());
        }
        let machines = self.local_hub_overview(daemon).await?.members;
        for conflict in conflicts {
            let message = conflict_message(&conflict, &machines);
            self.notify(
                daemon,
                About::named(None),
                AlertKind::SpaceConflict,
                message.to_string(),
            )
            .await;
        }
        Ok(())
    }

    /// Record that the machine `member` claims the space `id`, which it
    /// imports as another machine moves it there, and return the number
    /// the space keeps there, if the directory lists it; refused for the
    /// `ticket` of a move through the hub that its source settled, and for
    /// a `takeover` of the space from the package the hub keeps of it once
    /// the space's machine is connected again.
    pub(super) async fn record_space_claim(
        &self,
        member: &MemberId,
        id: &SpaceId,
        (ticket, takeover): (Option<&MoveTicket>, bool),
    ) -> Result<Option<SpaceNumber>> {
        let (member, id, ticket) = (member.clone(), id.clone(), ticket.cloned());
        let connected = self.links.connected();
        let handed = self.handovers.handed_to(&id, &member);
        self.store
            .write(move |tx| {
                if let Some(ticket) = &ticket {
                    fences::ensure_unsettled(tx, ticket)?;
                }
                if takeover {
                    takeovers::record_takeover(tx, &id, &member, (&connected, handed))?;
                }
                records::claim(tx, &member, &id)?;
                numbers::keep_for_claim(tx, &member, &id)
            })
            .await
    }

    /// Settle the move `ticket` of the space `id` to the machine `member`:
    /// whether that machine claimed it; when not, its later claims with the
    /// ticket are refused.
    pub(super) async fn record_settled_move(
        &self,
        id: &SpaceId,
        member: &MemberId,
        ticket: &MoveTicket,
    ) -> Result<bool> {
        let (id, member, ticket) = (id.clone(), member.clone(), ticket.clone());
        (self.store)
            .write(move |tx| fences::settle(tx, &id, &member, &ticket))
            .await
    }

    /// The directory this daemon keeps as the hub: its machines, with
    /// whether each is connected and when it was last seen, and their
    /// spaces, without the repositories [`Self::local_repositories`] lists.
    pub(super) async fn local_directory(&self, daemon: &dyn Daemon) -> Result<Directory> {
        let machines = self.local_machines(daemon).await?;
        let (spaces, conflicts) = self.store.read(records::read).await?;
        Ok(Directory {
            machines,
            spaces,
            conflicts,
            ..Directory::default()
        })
    }

    /// The directory this daemon keeps as the hub without its spaces: its
    /// machines and the repositories each registers.
    pub(super) async fn local_repositories(&self, daemon: &dyn Daemon) -> Result<Directory> {
        let machines = self.local_machines(daemon).await?;
        let repositories = self.store.read(repositories::read).await?;
        Ok(Directory {
            machines,
            repositories,
            ..Directory::default()
        })
    }

    /// The machines this daemon's directory lists as the hub, with whether
    /// each is connected and when it was last seen.
    async fn local_machines(&self, daemon: &dyn Daemon) -> Result<Vec<Member>> {
        let members = self.local_hub_overview(daemon).await?.members;
        Ok((members.into_iter())
            .filter(|member| member.kind == MemberKind::Machine)
            .collect())
    }

    /// The machine that owns the space `id` in this hub's directory, as
    /// `asker` sees it: none while `asker` claims the space, which it opens
    /// as another machine moves it there.
    pub(super) async fn local_space_owner(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        asker: &MemberId,
    ) -> Result<Option<Member>> {
        let (space, asker) = (id.clone(), asker.clone());
        let owner = (self.store).read(move |tx| records::owner_seen_by(tx, &space, &asker));
        let Some(owner) = owner.await? else {
            return Ok(None);
        };
        let members = self.local_hub_overview(daemon).await?.members;
        Ok(members.into_iter().find(|member| member.id == owner))
    }
}

/// The alert that `conflict` raised, naming the command that resolves it on
/// the machine of the copy.
fn conflict_message(conflict: &Conflict, machines: &[Member]) -> String {
    let name = |id: &MemberId| {
        (machines.iter().find(|machine| &machine.id == id))
            .map_or_else(|| id.to_string(), |machine| machine.name.clone())
    };
    let (space, copy, owner) = (&conflict.space, name(&conflict.copy), name(&conflict.owner));
    format!(
        "Machine {copy} has space {} ({space}), which runs on {owner} as well; the hub lists and routes to {owner}'s only. On {copy}, remove its copy with shoal rm {space}, or keep it as a new space with shoal start --copy {space} first",
        conflict.name,
    )
}
