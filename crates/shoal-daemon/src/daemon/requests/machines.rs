//! The daemon's answers to [`MachineMethod`] requests, which only the user
//! sends, since each acts on every space of a machine.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::{
    protocol::{Body, MachineMethod},
    refusal::{Reason, Refusal},
};

pub(super) async fn handle(manager: &Arc<Manager>, method: MachineMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        MachineMethod::PauseMachine { machine } => {
            Body::MachinePaused(manager.pause(machine.as_deref()).await?)
        }
        MachineMethod::UnpauseMachine { machine } => {
            Body::MachineUnpaused(manager.unpause(machine.as_deref()).await?)
        }
    }))
}

impl Checked for MachineMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::PauseMachine { .. } | Self::UnpauseMachine { .. } => None,
        }
    }
}

impl Scoped for MachineMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Err(Refusal::new(
            Reason::NotReady,
            "only the user pauses and unpauses a machine, since that acts on every space there",
        )
        .into())
    }
}

#[cfg(test)]
mod tests;
