//! The messages and leftovers an agent receives beside its items'
//! activity, as links deliver them.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::alerts::Unread;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::Messages;
use shoal_protocol::processes::Leftover;
use tokio::sync::watch;

use crate::{
    daemon::{alerts::Alert, manager::Manager},
    model::Space,
};

impl Messages for Manager {
    fn unread_messages<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Unread>> {
        Box::pin(self.agents.unread_messages(id))
    }

    fn deliverable_messages<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(self.agents.deliverable_messages(self, space))
    }

    fn settled_messages<'a>(
        &'a self,
        space: &'a Space,
        messages: Vec<Alert>,
    ) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(self.agents.settled_messages(self, space, messages))
    }

    fn undelivered_leftovers(&self, id: &SpaceId, room: usize) -> Result<Vec<Leftover>> {
        self.spaces.leftovers.undelivered(id, room)
    }

    fn watch_messages(&self) -> watch::Receiver<u64> {
        self.signals.watch_messages()
    }

    fn message_queued(&self) {
        self.signals.message_queued();
    }
}
