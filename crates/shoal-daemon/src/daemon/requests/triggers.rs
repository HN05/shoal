//! The daemon's answers to [`TriggerMethod`] requests, and who may send
//! them.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, TriggerMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: TriggerMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        TriggerMethod::ListTriggers { context } => {
            Body::Triggers(manager.list_triggers(context).await?)
        }
        TriggerMethod::PauseTrigger {
            context,
            name,
            paused,
        } => Body::Trigger(
            manager
                .pause_trigger(context, &name, paused, caller)
                .await?,
        ),
        TriggerMethod::RunTrigger {
            context,
            name,
            request,
        } => Body::TriggerFired(manager.run_trigger(context, &name, request, caller).await?),
    }))
}

impl Scoped for TriggerMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        // Listing is open to every caller, and the `triggers` permission
        // decides the rest, whichever repository the request names, since
        // a trigger acts for the user rather than in the caller's tree.
        Ok(None)
    }
}
