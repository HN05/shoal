//! The alerts that tell of agents.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Alerts;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use crate::daemon::{manager::Manager, notifications::AlertKind};

impl Alerts for Manager {
    fn notify_user(&self, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        Box::pin(self.notify(None, kind, message))
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

    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.report_state_failure(selector, error))
    }
}
