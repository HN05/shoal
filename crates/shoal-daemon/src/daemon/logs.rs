//! The logs requests read: a space's, by any selector or, once it is
//! removed, by its last name or ID until its events expire, within the
//! caller's scope, and Shoal's own; and how long a removed space's events
//! stay.
use shoal_daemon_base::permitted_actions::Action;
use std::time::Duration;

use anyhow::{Result, ensure};
use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    log::{LogEntry, LogFilter, LogTopic},
    space_names::SpaceSelector,
};

use super::{
    events::{retention, shoal_log, space_log},
    log,
    manager::Manager,
    scope::Caller,
};
use crate::config::permissions::PermissionKey;

/// How far a removed space's log looks up for a space that still exists,
/// through the spaces removed above it.
const REMOVED_ANCESTORS: usize = 32;

impl Manager {
    /// The newest `limit` events of the log of the space `space` names, or
    /// of Shoal's without one, that `filter` shows, oldest first.
    pub(crate) async fn log(
        &self,
        space: Option<&SpaceSelector>,
        filter: LogFilter,
        limit: u32,
    ) -> Result<Vec<LogEntry>> {
        match space {
            Some(space) => self.space_log(space, filter, limit).await,
            None => shoal_log::read(&self.store, filter, limit).await,
        }
    }

    /// The newest `limit` events of the log of the space `selector` names
    /// that `filter` shows, oldest first.
    pub(crate) async fn space_log(
        &self,
        selector: &SpaceSelector,
        filter: LogFilter,
        limit: u32,
    ) -> Result<Vec<LogEntry>> {
        let id = self.log_space(selector).await?;
        space_log::read(&self.store, id, filter, limit).await
    }

    /// Point `selector` at the space whose log a scoped caller reads: its
    /// own or one below it, or a removed space that was below it. Shoal's
    /// log, without a space, is the user's.
    pub(in crate::daemon) async fn narrow_to_log(
        &self,
        caller: &Caller,
        selector: &mut Option<SpaceSelector>,
    ) -> Result<()> {
        let Some(selector) = selector else {
            let action = Action::new(PermissionKey::Notifications, "Shoal's log");
            return self.authorize_action(Some(caller), action).await;
        };
        let id = self.log_space(selector).await?;
        let reached = match self.nearest_existing(&id).await? {
            Some(space) => self.reaches(caller, &space).await?,
            None => self.reach_root(Some(caller)).await?.is_none(),
        };
        ensure!(
            reached,
            "workspace processes can only read the logs of their own space and those below it"
        );
        *selector = SpaceSelector::id(&id);
        Ok(())
    }

    /// `topic` naming its space by ID, so a rename, or a later space taking
    /// its name, does not change the log a subscription follows.
    pub(in crate::daemon) async fn pin_log(&self, topic: LogTopic) -> Result<LogTopic> {
        let Some(space) = &topic.space else {
            return Ok(topic);
        };
        let id = self.log_space(space).await?;
        Ok(LogTopic {
            space: Some(SpaceSelector::id(&id)),
            ..topic
        })
    }

    /// The space `id` when it exists, else the nearest one that existed
    /// above it when it was removed, through the spaces removed above it;
    /// none once that walk leaves the tree.
    async fn nearest_existing(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
        let mut space = id.clone();
        for _ in 0..REMOVED_ANCESTORS {
            if self.space_by_id(&space).await.is_ok() {
                return Ok(Some(space));
            }
            let removed = space.clone();
            match space_log::parent_at_removal(&self.store, removed).await? {
                Some(parent) => space = parent,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The ID of the space `selector` names: one that exists, else a
    /// removed one whose events the journal keeps.
    async fn log_space(&self, selector: &SpaceSelector) -> Result<SpaceId> {
        let error = match self.space(selector).await {
            Ok(space) => return Ok(space.id.clone()),
            Err(error) => error,
        };
        let removed = match selector.given_id().cloned() {
            Some(id) => space_log::removed(&self.store, id).await?,
            None => None,
        };
        removed.ok_or(error)
    }
}

impl Manager {
    /// Forget the events of each space gone for longer than its retention,
    /// and Shoal's own events older than the global retention.
    pub(crate) async fn expire_events(&self) -> Result<()> {
        retention::expire(&self.store, self).await
    }

    /// A consistent replay batch from a client's cursor, including an
    /// explicit expired or future one.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn space_events(
        &self,
        after: Option<super::events::EventCursor>,
        limit: u32,
    ) -> Result<Vec<super::events::EventItem>> {
        let (items, _) = super::events::page(&self.store, after, None, limit).await?;
        Ok(items)
    }
}

impl retention::Retention for Manager {
    /// Its settings' retention, or the global setting's for a chat without
    /// a repository or when the repository's cannot be read.
    fn of<'a>(&'a self, repository: Option<&'a RepositoryId>) -> BoxFuture<'a, Duration> {
        Box::pin(async move {
            let settings = self.space_settings_in(repository).await.or_else(|error| {
                log!("events of removed spaces kept by the global retention: {error:#}");
                self.config().resolve(&Default::default())
            });
            settings.map_or(Duration::ZERO, |settings| {
                settings.cleanup.restore.retention()
            })
        })
    }
}

#[cfg(test)]
mod tests;
