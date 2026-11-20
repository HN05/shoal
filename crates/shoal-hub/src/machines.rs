//! The joined machine a request names to act on, by its ID or a name only
//! one machine has, such as the one `shoal start --machine` starts a space
//! on.
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
    /// The joined machine `named` names, by its ID or a name only one
    /// machine has, as the hub lists them, or as this machine last learned
    /// them while the hub cannot be asked; refused on a machine in no hub.
    pub async fn named_machine(&self, daemon: &dyn Daemon, named: &str) -> Result<Named> {
        let Some((own, directory)) = self.directory_or_known(daemon).await? else {
            bail!("this machine joined no hub, so its spaces start here; leave out --machine");
        };
        let machine = (directory.named_machine(named))
            .map_err(|message| Refusal::new(Reason::NotFound, message))?
            .clone();
        let own = machine.id == own;
        Ok(Named { machine, own })
    }
}
