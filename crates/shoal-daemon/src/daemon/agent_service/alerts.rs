//! The alerts that tell of agents.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Alerts;

use crate::daemon::alerts::About;
use crate::daemon::approvals::ApprovalRequest;
use crate::daemon::manager::Manager;
use crate::daemon::notifications::AlertKind;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

impl Alerts for Manager {
    fn notify<'a>(
        &'a self,
        name: Option<&'a SpaceName>,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.notify(self, name, kind, message))
    }

    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(self.coordination.notify_about(self, about, kind, message))
    }

    fn notify_space<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        Box::pin(
            self.coordination
                .notify_space(self, name, id, kind, message),
        )
    }

    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.coordination.tell_space(self, id, kind, message))
    }

    fn notify_approval<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.notify_approval(self, request))
    }

    fn ask_user_about<'a>(&'a self, requests: &'a [ApprovalRequest]) -> BoxFuture<'a, ()> {
        Box::pin(self.coordination.ask_user_about(self, requests))
    }

    fn tell_still_blocked<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.tell_still_blocked(id))
    }

    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.coordination
                .report_state_failure(self, selector, error),
        )
    }
}
