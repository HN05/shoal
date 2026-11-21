//! The daemon's answers to [`TriggerMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, TriggerMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: TriggerMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        TriggerMethod::ListTriggers { context } => {
            Body::Triggers(manager.runner.list_triggers(&**manager, context).await?)
        }
        TriggerMethod::PauseTrigger {
            context,
            name,
            paused,
        } => Body::Trigger(
            (manager.runner)
                .pause_trigger(&**manager, context, &name, paused, caller)
                .await?,
        ),
        TriggerMethod::RunTrigger {
            context,
            name,
            request,
        } => {
            let daemon: Arc<dyn shoal_runner::daemon::Daemon> = manager.clone();
            let fired = (manager.runner).run_trigger(&daemon, context, &name, request, caller);
            Body::TriggerFired(fired.await?)
        }
    }))
}

impl Checked for TriggerMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ListTriggers { .. } | Self::PauseTrigger { .. } | Self::RunTrigger { .. } => None,
        }
    }
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
