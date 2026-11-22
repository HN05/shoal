//! The spaces agents run in.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Spaces;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{agent::Agent, config::Effective, daemon::manager::Manager, model::Space};

impl Spaces for Manager {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.space(selector))
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.space_by_id(id))
    }

    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.space_exists(id))
    }

    fn agent_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>> {
        Box::pin(self.agent_of(id))
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings(self, space))
    }
}
