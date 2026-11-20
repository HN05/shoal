//! The daemon's answers to [`ChangeMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Context as _, Result};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, ChangeMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChangeMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChangeMethod::RecordChange { change } => {
            let caller = caller.context("only an agent's hooks record its changes")?;
            Body::Changes(manager.record_tool_change(caller, change).await?)
        }
    }))
}

impl Scoped for ChangeMethod {
    /// A hook records the changes of its own space's agent.
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ChangeMethod::RecordChange { .. } => None,
        })
    }
}
