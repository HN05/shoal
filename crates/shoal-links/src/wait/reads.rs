//! The reads of item activity a watch, a watch's start and the background
//! poll make, as the user's own logins, and the word to the user when a
//! forge limits them.
use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};
use shoal_daemon_base::alerts::{About, NewAlert};
use shoal_forge::{
    account::Account,
    transport::limits::rate_limited,
    updates::{Snapshot, UpdateRequests},
};
use shoal_forge_types::items::{link::Selection, reference::ItemRef};
use shoal_protocol::alerts::AlertKind;

use crate::{Links, daemon::Daemon};

/// How many of one space's items a poll reads at once: each read runs curl
/// processes.
const ITEMS_READ_AT_ONCE: usize = 4;

impl Links {
    /// The activity of `items`, each read from the forge its URL names, so
    /// a worktree Git cannot read holds none of it back.
    pub async fn item_activity(
        &self,
        daemon: &dyn Daemon,
        items: Vec<ItemRef>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        let account = &self.user_account();
        let linked = selection.input.is_none();
        stream::iter(items)
            .map(|url| async move {
                let snapshot = (self.read_item_activity(daemon, account, &url, linked)).await?;
                Ok((url.to_string(), snapshot))
            })
            .buffered(ITEMS_READ_AT_ONCE)
            .try_collect()
            .await
    }

    /// The activity of each of `items` the forge reads in full, leaving out
    /// the others.
    pub async fn complete_reads(
        &self,
        daemon: &dyn Daemon,
        items: Vec<ItemRef>,
    ) -> Vec<(String, Snapshot)> {
        let account = &self.user_account();
        stream::iter(items)
            .map(|item| async move {
                let read = self
                    .read_item_activity(daemon, account, &item, false)
                    .await
                    .ok()?;
                let complete = read.errors().next().is_none();
                complete.then(|| (item.to_string(), read))
            })
            .buffered(ITEMS_READ_AT_ONCE)
            .filter_map(std::future::ready)
            .collect()
            .await
    }

    /// The activity of `item`. The forge failing to read a `linked` item
    /// leaves an unread snapshot, so a watch reports the failure as it
    /// reports one of the item's sources; an item named on the command line
    /// still fails, so a mistyped number does not wait silently.
    async fn read_item_activity(
        &self,
        daemon: &dyn Daemon,
        account: &Account,
        item: &ItemRef,
        linked: bool,
    ) -> Result<Snapshot> {
        let forge = daemon.item_forge(item).await?;
        match forge.item_activity(account, item).await {
            Err(error) if linked && rate_limited(&error).is_none() => {
                Ok(Snapshot::unread_item(item.kind(), &error))
            }
            read => read,
        }
    }

    /// Tell the user when `error` is a forge's rate limit, and whether it is.
    pub async fn notify_rate_limit(&self, error: &anyhow::Error) -> bool {
        let Some(limit) = rate_limited(error) else {
            return false;
        };
        let message = format!(
            "{} is limiting Shoal's requests until {}; polling waits until then",
            limit.host,
            limit.until.local_minutes()
        );
        let alert = NewAlert::shoal(AlertKind::ForgeLimited, About::default(), message);
        self.notifications.notify_user(alert).await;
        true
    }
}
