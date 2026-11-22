//! Alerts: one-way notices from the user, a space or Shoal, for the user or
//! a space's agent. Shoal's own reach a space's coordinator instead of the
//! user where it has one; [`routing`] decides whom every alert reaches.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

pub(crate) use shoal_daemon_base::alerts::{
    About, Alert, AlertKind, COLUMNS, FROM, NewAlert, Sender, Sent, rebase_attachments, row,
};

use super::{
    manager::Manager,
    notifications::{Coordinators, Source},
    workspace_files::SentFile,
};

mod reading;
mod routing;
mod sending;

pub(crate) use reading::users_alerts;
pub(crate) use routing::{Reader, Recipients, Route};
pub(crate) use sending::recipient;
#[cfg(test)]
pub(crate) use sending::sent_alert;

impl Manager {
    /// Record an event and wake watchers. Failures are logged, never returned:
    /// an alert of Shoal's must not fail the operation it describes.
    pub async fn notify(
        &self,
        workspace: Option<&SpaceName>,
        kind: AlertKind,
        message: impl Into<String>,
    ) {
        self.notify_about(About::named(workspace), kind, message.into())
            .await;
    }

    /// [`Manager::notify`] about the space `id`, which has `name`: names
    /// repeat, so only its identity says which space the event is about.
    pub async fn notify_space(
        &self,
        name: &SpaceName,
        id: &SpaceId,
        kind: AlertKind,
        message: impl Into<String>,
    ) {
        (self.notify_about(About::space(name, id), kind, message.into())).await;
    }

    /// [`Manager::notify`] about a workspace whose identity the caller holds,
    /// so a rename meanwhile cannot attribute the event to another one.
    pub(crate) async fn notify_about(&self, about: About, kind: AlertKind, message: String) {
        self.notify_with_files(about, kind, message, &[]).await;
    }

    /// [`Manager::notify_about`] with the files the space sent, which a
    /// coordinating agent receives as the alert's attached files.
    pub(crate) async fn notify_with_files(
        &self,
        about: About,
        kind: AlertKind,
        message: String,
        files: &[SentFile],
    ) {
        let alert = NewAlert {
            attachments: shoal_protocol::files::paths_here(files),
            ..NewAlert::shoal(kind, about, message)
        };
        self.notifications.notify(self, alert).await;
    }

    /// Tell the agent coordinating the space `about` names about an event,
    /// or record it for the user; where it went.
    pub(crate) async fn record_notification(
        &self,
        about: About,
        kind: AlertKind,
        message: impl Into<String>,
    ) -> Result<Sent> {
        (self.record_sent(NewAlert::shoal(kind, about, message.into()))).await
    }

    /// Tell the agent coordinating the space `alert` names about it, or
    /// record it for the user; where it went.
    pub(crate) async fn record_sent(&self, alert: NewAlert) -> Result<Sent> {
        self.notifications.record(self, alert).await
    }

    /// Tell the user that a turn-state report of the space's agent failed
    /// with `error`, so the state shown for the agent may be out of date.
    pub async fn report_state_failure(&self, selector: &SpaceSelector, error: &str) -> Result<()> {
        let space = self.space(selector).await?;
        let about = About::space(&space.name, &space.id);
        let error = error.replace(char::is_control, " ");
        let message = crate::validate::shorten(
            format!("agent state report failed; {error}"),
            crate::validate::MAX_MESSAGE_LEN,
        );
        self.record_notification(about, AlertKind::StateReportFailed, message)
            .await?;
        Ok(())
    }
}

impl Coordinators for Manager {
    fn tell<'a>(
        &'a self,
        alert: NewAlert,
        for_user: bool,
        source: Source,
    ) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(self.tell_coordinator(alert, for_user, source))
    }
}

#[cfg(test)]
mod tests;
