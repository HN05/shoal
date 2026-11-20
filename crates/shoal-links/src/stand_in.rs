//! A stand-in for the rest of the daemon, which the links' tests build
//! their operations over: the issues and PRs a test assigns to each space,
//! and nothing else.
use std::collections::HashMap;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_daemon_base::{callers::Caller, permitted_actions::Action};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Selected, Space},
    protocol::ItemContext,
    space_names::SpaceSelector,
};

use crate::{
    daemon::{Forges, Permissions, Spaces},
    space::LinkSpace,
};

/// The daemon as the links' tests see it.
#[derive(Default)]
pub struct StandIn {
    /// Each space's assigned issues and PRs, in the order assigned.
    pub assigned: HashMap<SpaceId, Vec<ItemRef>>,
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the links' stand-in has no repositories, spaces or forges";

impl Spaces for StandIn {
    fn repository<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn space<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn context_space<'a>(&'a self, _: &'a ItemContext) -> BoxFuture<'a, Result<LinkSpace>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn verify_space<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn linked_items<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: ItemKind,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        let items = (self.assigned.get(id).into_iter().flatten())
            .filter(|item| item.kind() == kind.into())
            .cloned()
            .collect();
        Box::pin(async { Ok(items) })
    }

    fn select_items<'a>(
        &'a self,
        _: &'a LinkSpace,
        _: &'a Selection,
        _: bool,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Forges for StandIn {
    fn resolve_forge<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn resolve_named<'a>(
        &'a self,
        _: &'a ForgeRepo,
        _: &'a str,
    ) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn item_forge<'a>(&'a self, _: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Permissions for StandIn {
    fn authorize_action<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
