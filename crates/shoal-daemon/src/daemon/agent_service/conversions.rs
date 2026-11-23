//! The conversions that hand a space's agent over to its new place.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Conversions;

use crate::daemon::{conversions::ConversionStop, manager::Manager};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Conversions for Manager {
    fn stop_converting_agent<'a>(
        &'a self,
        id: &'a SpaceId,
        holds: bool,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(
            self.spaces
                .stop_converting_agent(self, id, ConversionStop { holds }),
        )
    }

    fn ensure_takes_agents<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.ensure_takes_agents(selector))
    }

    fn hand_over_before_removal<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.hand_over_before_removal(self, id))
    }
}
