//! A space whose agent a restart acts on, of this machine or of another
//! joined machine, as the hub lists it.
use shoal_ids::{
    repository_ids::RepositoryId,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::{
    hub::{Member, directory::Listed},
    space_names::{SpaceName, SpaceSelector},
};

use crate::model::Space;

#[derive(Debug, Clone)]
pub(super) struct Target {
    pub id: SpaceId,
    pub number: SpaceNumber,
    pub name: SpaceName,
    /// The other machine it runs on; none for this machine's.
    pub machine: Option<Member>,
    /// Its repository, for a space of this machine, which checks the
    /// permissions a restart gives there; another machine checks its own.
    pub repository: Option<RepositoryId>,
    /// Whether the daemon may keep a restart of it for later: one that this
    /// machine lists with one, or any of another machine's, whose listing
    /// does not say.
    pub may_keep_restart: bool,
}

impl Target {
    /// The space of another machine that `listed` lists on `machine`.
    pub(super) fn elsewhere(listed: &Listed, machine: Member) -> Self {
        let space = &listed.space;
        Self {
            id: space.id.clone(),
            number: space.number,
            name: space.name.clone(),
            machine: Some(machine),
            repository: None,
            may_keep_restart: true,
        }
    }

    pub(super) fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.id)
    }

    /// The name of the other machine it runs on.
    pub(super) fn machine_name(&self) -> Option<String> {
        self.machine.as_ref().map(|machine| machine.name.clone())
    }

    /// Its name, with the other machine it runs on, as reports show it.
    pub(super) fn shown(&self) -> String {
        on_machine(&self.name, self.machine_name().as_deref())
    }
}

/// The space `name`, with the other machine `machine` it runs on, as
/// reports show it.
pub(super) fn on_machine(name: &str, machine: Option<&str>) -> String {
    match machine {
        Some(machine) => format!("{name} on {machine}"),
        None => name.to_owned(),
    }
}

impl From<Space> for Target {
    fn from(space: Space) -> Self {
        Self {
            may_keep_restart: space.restart.is_some(),
            id: space.id,
            number: space.number,
            name: space.name,
            machine: None,
            repository: space.repository_id,
        }
    }
}
