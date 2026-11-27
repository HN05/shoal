//! The joined machine a request names to act on.
use anyhow::{Result, bail};
use shoal_protocol::hub::Member;

use crate::{
    Hub,
    daemon::Daemon,
    refusal::{Reason, Refusal},
};

/// A joined machine a request names.
pub struct Named {
    pub machine: Member,
    /// Whether it is this machine.
    pub own: bool,
}

impl Hub {
    /// The joined machine `named` names, by its ID or a name only one machine
    /// has. Reads the hub's list, or what this machine last learned while the
    /// hub cannot be asked. Refused on a machine in no hub.
    pub async fn named_machine(&self, daemon: &dyn Daemon, named: &str) -> Result<Named> {
        let Some((own, directory)) = self.directory_or_known(daemon).await? else {
            bail!("this machine joined no hub, so --machine names no other machine; leave it out");
        };
        let machine = (directory.named_machine(named))
            .map_err(|message| Refusal::new(Reason::NotFound, message))?
            .clone();
        let own = machine.id == own;
        Ok(Named { machine, own })
    }

    /// The joined machines, as [`Self::named_machine`] reads them; none on a
    /// machine in no hub.
    pub async fn joined_machines(&self, daemon: &dyn Daemon) -> Result<Vec<Member>> {
        let known = self.directory_or_known(daemon).await?;
        Ok(known
            .map(|(_, directory)| directory.machines)
            .unwrap_or_default())
    }

    /// The other joined machine `named` names, as
    /// [`Self::named_machine`] reads it; none when it names none, or this
    /// one.
    pub async fn other_machine(
        &self,
        daemon: &dyn Daemon,
        named: Option<&str>,
    ) -> Result<Option<Member>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let Named { machine, own } = self.named_machine(daemon, named).await?;
        Ok((!own).then_some(machine))
    }
}
