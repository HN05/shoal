//! The reads of item activity a watch, a watch's start and the background
//! poll make, as the user's own logins, and the word to the user when a
//! forge limits them.
use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};
use shoal_daemon_base::alerts::{About, NewAlert};
use shoal_forge::{
    activity::ActivityRequests, transport::account::Account, transport::limits::rate_limited,
    updates::Snapshot,
};
use shoal_forge_types::items::{link::Selection, reference::ItemRef};
use shoal_protocol::alerts::AlertKind;

use crate::{Links, daemon::Daemon};

/// How many of one space's items a poll reads at once: each read runs curl
/// processes.
const ITEMS_READ_AT_ONCE: usize = 4;

/// An item a poll reads, with the activity its space already recorded of
/// it, which spares the read what that holds.
pub struct Lookup {
    pub item: ItemRef,
    pub known: Option<Snapshot>,
}

impl From<ItemRef> for Lookup {
    fn from(item: ItemRef) -> Self {
        Self { item, known: None }
    }
}

impl Links {
    /// The activity of `items`, each read from the forge its URL names, so
    /// a worktree Git cannot read holds none of it back.
    pub async fn item_activity(
        &self,
        daemon: &dyn Daemon,
        items: Vec<Lookup>,
        selection: &Selection,
    ) -> Result<Vec<(String, Snapshot)>> {
        let account = &self.user_account();
        let linked = selection.input.is_none();
        stream::iter(items)
            .map(|lookup| async move {
                let snapshot = self
                    .read_item_activity(daemon, account, &lookup, linked)
                    .await?;
                Ok((lookup.item.to_string(), snapshot))
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
        stream::iter(items.into_iter().map(Lookup::from))
            .map(|lookup| async move {
                let read = self
                    .read_item_activity(daemon, account, &lookup, false)
                    .await
                    .ok()?;
                let complete = read.errors().next().is_none();
                complete.then(|| (lookup.item.to_string(), read))
            })
            .buffered(ITEMS_READ_AT_ONCE)
            .filter_map(std::future::ready)
            .collect()
            .await
    }

    /// The activity of `lookup`'s item. The forge failing to read a
    /// `linked` item leaves an unread snapshot, so a watch reports the
    /// failure as it reports one of the item's sources; an item named on
    /// the command line still fails, so a mistyped number does not wait
    /// silently.
    async fn read_item_activity(
        &self,
        daemon: &dyn Daemon,
        account: &Account,
        lookup: &Lookup,
        linked: bool,
    ) -> Result<Snapshot> {
        let item = &lookup.item;
        let forge = daemon.item_forge(item).await?;
        match forge
            .item_activity(account, item, lookup.known.as_ref())
            .await
        {
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
