//! The registered repositories and the spaces requests about items come
//! from, and the watches of their linked items, as links reach them.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_links::{
    daemon::{SpaceAgent, Spaces, Watches},
    space::LinkSpace,
};

use crate::{
    config::Effective,
    daemon::manager::Manager,
    forge::items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
    model::{Repository, Selected, Space},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces for Manager {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(Manager::repository(self, id))
    }

    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space(self, selector))
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(Manager::find_space(self, id))
    }

    fn verify_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::verify_space(self, space))
    }

    fn linked_items<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: ItemKind,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        let id = id.to_owned();
        Box::pin(
            self.store
                .run(move |db| shoal_links::linked::assigned_items(db, &id, Some(kind))),
        )
    }

    fn select_items<'a>(
        &'a self,
        space: &'a LinkSpace,
        selection: &'a Selection,
        elsewhere: bool,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(Manager::select_items(self, space, selection, elsewhere))
    }

    fn acting_settings<'a>(&'a self, space: &'a LinkSpace) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(Manager::acting_settings(self, space))
    }

    fn space_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>> {
        Box::pin(Manager::issue_agent(self, id))
    }

    fn wake_cleanup(&self) {
        self.signals.wake_cleanup();
    }
}

impl Watches for Manager {
    fn start_linked_watches_gated<'a>(
        &'a self,
        id: &'a SpaceId,
        items: Vec<ItemRef>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(Manager::start_linked_watches_gated(self, id, items))
    }
}
