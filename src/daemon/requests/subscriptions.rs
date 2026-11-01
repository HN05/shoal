//! The daemon's answers to [`SubscriptionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    subscriptions,
    workspace::Manager,
};
use crate::protocol::SubscriptionMethod;

pub(super) fn handle(
    manager: &Arc<Manager>,
    method: SubscriptionMethod,
    caller: Option<&Caller>,
    scope: Option<&str>,
) -> Result<Answer> {
    let SubscriptionMethod::Subscribe { topics } = method;
    let subscriber = subscriptions::Subscriber {
        manager: manager.clone(),
        caller: caller.cloned(),
        scope: scope.map(str::to_owned),
    };
    Ok(Answer::connection(move |stream, request_id, guard| {
        subscriber.run(stream, request_id, topics, guard)
    }))
}

/// A scoped caller subscribes to what its scope reaches, which the
/// subscription narrows to it.
impl Scoped for SubscriptionMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(None)
    }
}
