//! The hub, as links pass the item activity of this machine's spaces up
//! through it to their coordinators on other machines.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::space_ids::SpaceId;
use shoal_links::daemon::Elsewhere;
use shoal_protocol::model::SpaceRef;

use crate::daemon::{alerts::AlertKind, hub::Event, manager::Manager};

impl Elsewhere for Manager {
    fn reaches_hub(&self) -> bool {
        self.hub.reaches_hub(self)
    }

    fn coordinator_elsewhere<'a>(&'a self, below: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(self.hub.coordinator_elsewhere(self, below))
    }

    fn pass_up_activity<'a>(
        &'a self,
        above: SpaceId,
        space: &'a SpaceRef,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        let event = Event {
            name: &space.name,
            space: &space.id,
            kind: AlertKind::ItemActivity,
            for_user: false,
            message,
            agent: None,
            files: &[],
        };
        Box::pin(self.hub.pass_up_now(self, above, event))
    }
}
