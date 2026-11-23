//! Alerts: one-way notices from the user, a space or Shoal, for the user or
//! a space's agent. Shoal's own reach a space's coordinator instead of the
//! user where it has one; [`routing`] decides whom every alert reaches.
use crate::{Coordination, daemon::Daemon};
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

pub use shoal_daemon_base::alerts::{
    About, Alert, AlertKind, COLUMNS, FROM, NewAlert, Sender, Sent, rebase_attachments, row,
};

use shoal_protocol::files::SentFile;

/// What a message from the chat says without the agent's own text, as
/// from an agent whose hook does not pass its turn's final message.
pub const WROTE_IN_CHAT: &str = "The agent wrote in its chat; attach to its session to read it.";

mod reading;
mod routing;
mod sending;

pub use reading::users_alerts;
pub use routing::{Reader, Recipients, Route};
pub use sending::sent_alert;
pub use sending::{recipient, send_with_files};

impl Coordination {
    /// Record an event and wake watchers. Failures are logged, never returned:
    /// an alert of Shoal's must not fail the operation it describes.
    pub async fn notify(
        &self,
        daemon: &dyn Daemon,
        workspace: Option<&SpaceName>,
        kind: AlertKind,
        message: impl Into<String>,
    ) {
        self.notify_about(daemon, About::named(workspace), kind, message.into())
            .await;
    }

    /// [`Coordination::notify`] about the space `id`, which has `name`: names
    /// repeat, so only its identity says which space the event is about.
    pub async fn notify_space(
        &self,
        daemon: &dyn Daemon,
        name: &SpaceName,
        id: &SpaceId,
        kind: AlertKind,
        message: impl Into<String>,
    ) {
        (self.notify_about(daemon, About::space(name, id), kind, message.into())).await;
    }

    /// [`Coordination::notify`] about a workspace whose identity the caller holds,
    /// so a rename meanwhile cannot attribute the event to another one.
    pub async fn notify_about(
        &self,
        daemon: &dyn Daemon,
        about: About,
        kind: AlertKind,
        message: String,
    ) {
        self.notify_with_files(daemon, about, kind, message, &[])
            .await;
    }

    /// [`Coordination::notify_about`] with the files the space sent, which a
    /// coordinating agent receives as the alert's attached files.
    pub async fn notify_with_files(
        &self,
        daemon: &dyn Daemon,
        about: About,
        kind: AlertKind,
        message: String,
        files: &[SentFile],
    ) {
        let alert = NewAlert::shoal(kind, about, message).with_files(files);
        self.notifications.notify(daemon, alert).await;
    }

    /// Tell the agent coordinating the space `about` names about an event,
    /// or record it for the user; where it went.
    pub async fn record_notification(
        &self,
        daemon: &dyn Daemon,
        about: About,
        kind: AlertKind,
        message: impl Into<String>,
    ) -> Result<Sent> {
        (self.record_sent(daemon, NewAlert::shoal(kind, about, message.into()))).await
    }

    /// Tell the agent coordinating the space `alert` names about it, or
    /// record it for the user; where it went.
    pub async fn record_sent(&self, daemon: &dyn Daemon, alert: NewAlert) -> Result<Sent> {
        self.notifications.record(daemon, alert).await
    }

    /// Tell the user that a turn-state report of the space's agent failed
    /// with `error`, so the state shown for the agent may be out of date.
    pub async fn report_state_failure(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        error: &str,
    ) -> Result<()> {
        let space = daemon.space(selector).await?;
        let about = About::space(&space.name, &space.id);
        let error = error.replace(char::is_control, " ");
        let message = crate::validate::shorten(
            format!("agent state report failed; {error}"),
            crate::validate::MAX_MESSAGE_LEN,
        );
        self.record_notification(daemon, about, AlertKind::StateReportFailed, message)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
