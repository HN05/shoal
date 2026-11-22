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
        Box::pin(self.notify(name, kind, message))
    }

    fn notify_about(&self, about: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(self.notify_about(about, kind, message))
    }

    fn notify_space<'a>(
        &'a self,
        name: &'a SpaceName,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.notify_space(name, id, kind, message))
    }

    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.tell_space(id, kind, message))
    }

    fn notify_approval<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.notify_approval(request))
    }

    fn ask_user_about<'a>(&'a self, requests: &'a [ApprovalRequest]) -> BoxFuture<'a, ()> {
        Box::pin(self.ask_user_about(requests))
    }

    fn tell_still_blocked<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.tell_still_blocked(id))
    }

    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.report_state_failure(selector, error))
    }
}
