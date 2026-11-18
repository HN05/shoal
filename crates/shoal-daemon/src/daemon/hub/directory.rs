//! The hub's directory of every joined machine's spaces: the reports each
//! machine sends of its own, the view the hub serves from them, and the
//! machine that owns each space ID.
use anyhow::Result;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    Member, MemberKind,
    directory::{Conflict, Directory, Report},
};

use crate::daemon::{notifications::NotificationKind, workspace::Manager};

mod records;

pub(super) use records::forget;

impl Manager {
    /// Record the machine `member`'s `report` of its spaces, alerting the
    /// user to each space ID it reported that another machine owns.
    pub(super) async fn record_report(&self, member: &MemberId, report: Report) -> Result<()> {
        let member = member.clone();
        let conflicts = (self.store)
            .write(move |tx| records::apply(tx, &member, report))
            .await?;
        if conflicts.is_empty() {
            return Ok(());
        }
        let machines = self.local_hub_overview().await?.members;
        for conflict in conflicts {
            let message = conflict_message(&conflict, &machines);
            self.notify(None, NotificationKind::SpaceConflict, message)
                .await;
        }
        Ok(())
    }

    /// The directory this daemon keeps as the hub: its machines, with
    /// whether each is connected and when it was last seen, and their
    /// spaces.
    pub(super) async fn local_directory(&self) -> Result<Directory> {
        let members = self.local_hub_overview().await?.members;
        let (spaces, conflicts) = self.store.read(records::read).await?;
        let machines = (members.into_iter())
            .filter(|member| member.kind == MemberKind::Machine)
            .collect();
        Ok(Directory {
            machines,
            spaces,
            conflicts,
        })
    }

    /// The machine that owns the space `id` in this hub's directory.
    pub(super) async fn local_space_owner(&self, id: &SpaceId) -> Result<Option<Member>> {
        let space = id.clone();
        let Some(owner) = self
            .store
            .read(move |tx| records::owner(tx, &space))
            .await?
        else {
            return Ok(None);
        };
        let members = self.local_hub_overview().await?.members;
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
