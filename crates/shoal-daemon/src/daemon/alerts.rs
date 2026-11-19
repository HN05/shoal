//! What the daemon and the spaces tell the user: the events the daemon
//! records, which reach a space's coordinator instead where it has one, and
//! the messages agents send, with their files.
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::{
    events,
    manager::Manager,
    notifications::{About, Alert, AlertKind, Coordinators, Sent, Source},
    workspace_files::{self, Attacher, FileSource, NewFile, SentFile},
};

impl Manager {
    /// Record an event and wake watchers. Failures are logged, never returned:
    /// a notification must not fail the operation it describes.
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
        (self.notifications)
            .notify(self, about, kind, message, &[])
            .await;
    }

    /// Record a message a space's agent sends the user, naming the space,
    /// with copies of the files `attachments`, which the space's attached
    /// files keep as attached by `attacher` for as long as its results.
    /// Unlike daemon events, the message is the whole operation, so failures
    /// are returned.
    pub async fn send_message(
        &self,
        selector: &SpaceSelector,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Sent> {
        crate::validate::message(&message)?;
        workspace_files::check_sent(&attachments)?;
        let space = self.space(selector).await?;
        let about = About::space(&space.name, &space.id);
        let id = space.id.clone();
        let (kind, text) = (AlertKind::AgentMessage, message.clone());
        let notification = (self.send_with_files(
            &space.into(),
            FileSource::Alert,
            &text,
            attachments,
            attacher,
            |files: Vec<SentFile>| async move {
                (self.notifications)
                    .record(self, about, kind, message, &files)
                    .await
            },
        ))
        .await?;
        events::record_alert(&self.store, &id, text).await;
        Ok(Sent { notification })
    }

    /// The workspace or running chat `selector` names, as its messages to
    /// the user name their sender.
    async fn space_about(&self, selector: &SpaceSelector) -> Result<About> {
        let space = self.space(selector).await?;
        Ok(About::space(&space.name, &space.id))
    }

    /// Tell the user that a turn-state report of the space's agent failed
    /// with `error`, so the state shown for the agent may be out of date.
    pub async fn report_state_failure(&self, selector: &SpaceSelector, error: &str) -> Result<()> {
        let about = self.space_about(selector).await?;
        let error = error.replace(char::is_control, " ");
        let message = crate::validate::shorten(
            format!("agent state report failed; {error}"),
            crate::validate::MAX_MESSAGE_LEN,
        );
        let kind = AlertKind::StateReportFailed;
        (self.notifications)
            .record(self, about, kind, message, &[])
            .await?;
        Ok(())
    }

    /// Delete the message `id` the space sent the user, which the user need
    /// not see after all. A shown message stays; a correction follows it.
    pub async fn withdraw_message(&self, selector: &SpaceSelector, id: i64) -> Result<Alert> {
        let space = self.space_about(selector).await?;
        self.notifications.withdraw_message(space, id).await
    }
}

impl Coordinators for Manager {
    fn tell<'a>(
        &'a self,
        name: &'a SpaceName,
        id: Option<&'a SpaceId>,
        kind: AlertKind,
        message: String,
        files: Vec<PathBuf>,
        source: Source,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.tell_coordinator(name, id, Some(kind), message, files, source))
    }
}

#[cfg(test)]
mod tests;
