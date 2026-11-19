//! The joined machine a request names to act on, by its ID or a name only
//! one machine has, such as the one `shoal start --machine` starts a space
//! on.
use anyhow::{Result, bail};
use shoal_protocol::hub::{Member, MemberKind};

use crate::{
    daemon::manager::Manager,
    refusal::{Reason, Refusal},
};

/// A joined machine a request names.
pub(crate) struct Named {
    pub machine: Member,
    /// Whether it is this machine.
    pub own: bool,
}

impl Manager {
    /// The joined machine `named` names, by its ID or a name only one
    /// machine has, as the hub lists them, or as this machine last learned
    /// them while the hub cannot be asked; refused on a machine in no hub.
    pub(crate) async fn named_machine(&self, named: &str) -> Result<Named> {
        let Some((own, directory)) = self.directory_or_known().await? else {
            bail!("this machine joined no hub, so its spaces start here; leave out --machine");
        };
        let machine = select(directory.machines, named)?;
        let own = machine.id == own;
        Ok(Named { machine, own })
    }
}

/// The machine among `members` that `named` names, by its ID or a name
/// only one machine has, whatever devices are named.
fn select(members: Vec<Member>, named: &str) -> Result<Member> {
    let machines: Vec<Member> = (members.into_iter())
        .filter(|member| member.kind == MemberKind::Machine)
        .collect();
    if let Some(machine) = machines.iter().find(|machine| machine.id.as_str() == named) {
        return Ok(machine.clone());
    }
    let mut found = machines.into_iter().filter(|machine| machine.name == named);
    let message = match (found.next(), found.next()) {
        (Some(machine), None) => return Ok(machine),
        (Some(_), Some(_)) => {
            format!("several machines are named {named}; name one by the ID shoal hub ls shows")
        }
        (None, _) => format!("the hub has no machine {named}; shoal hub ls lists them"),
    };
    Err(Refusal::new(Reason::NotFound, message).into())
}

#[cfg(test)]
mod tests;
