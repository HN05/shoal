//! The daemon's answers to [`SubscriptionMethod`] requests, who may send them,
//! and what each requires of the space it acts on.
use shoal_protocol::scope_tokens::ScopeToken;
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    subscriptions,
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::SubscriptionMethod;
use shoal_protocol::space_names::SpaceSelector;

pub(super) fn handle(
    manager: &Arc<Manager>,
    method: SubscriptionMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    let SubscriptionMethod::Subscribe { topics, log } = method;
    let subscriber = subscriptions::Subscriber {
        manager: manager.clone(),
        caller: caller.cloned(),
        scope: scope.cloned(),
    };
    Ok(Answer::connection(move |stream, request_id, guard| {
        subscriber.run(stream, request_id, topics, log, guard)
    }))
}

impl Checked for SubscriptionMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::Subscribe { .. } => None,
        }
    }
}

/// A scoped caller subscribes to what its scope reaches, which the
/// subscription narrows to it, and to the log of a space it may read.
impl Scoped for SubscriptionMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        let SubscriptionMethod::Subscribe { log, .. } = self;
        if let Some(log) = log {
            manager.narrow_to_log(caller, &mut log.space).await?;
        }
        Ok(None)
    }
}
