//! A stand-in for the rest of the daemon, which the links' tests build
//! their operations over: the issues and PRs a test assigns to each space,
//! and nothing else.
use std::collections::HashMap;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_core::config::Effective;
use shoal_daemon_base::{
    alerts::{Alert, NewAlert, Sent, Unread},
    callers::Caller,
    notifications::{Coordinators, Source},
    permitted_actions::Action,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{ConflictCheck, LinkedItem, Repository, Selected, Space, SpaceRef, Workspace},
    processes::Leftover,
    space_names::SpaceSelector,
};
use tokio::sync::watch;

use crate::{
    daemon::{Elsewhere, Forges, Messages, Permissions, Runs, SpaceAgent, Spaces, Watches},
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

    fn find_space<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(async { Ok(None) })
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

    fn acting_settings<'a>(&'a self, _: &'a LinkSpace) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn space_settings<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn workspace_settings<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn space_agent<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn wake_cleanup(&self) {}

    fn registered_named<'a>(
        &'a self,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(async { Ok(None) })
    }

    fn tracked_agents(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        Box::pin(async { Vec::new() })
    }

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn watched_conflicts<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: bool,
    ) -> BoxFuture<'a, Result<Option<ConflictCheck>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Runs for StandIn {
    fn linked_run<'a>(
        &'a self,
        _: &'a LinkSpace,
        _: &'a ItemInput,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<LinkedItem>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Watches for StandIn {
    fn item_updates_queued(&self) {}

    fn watch_item_updates(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }

    fn agent_changed(&self) {}

    fn item_merged(&self, _: &RepositoryId) {}

    fn draft_changed(&self, _: &SpaceId) {}
}

impl Messages for StandIn {
    fn unread_messages<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Unread>> {
        Box::pin(async { Ok(Unread::default()) })
    }

    fn deliverable_messages<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn settled_messages<'a>(
        &'a self,
        _: &'a Space,
        messages: Vec<Alert>,
    ) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(async { Ok(messages) })
    }

    fn undelivered_leftovers(&self, _: &SpaceId, _: usize) -> Result<Vec<Leftover>> {
        Ok(Vec::new())
    }

    fn watch_messages(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }
}

impl Elsewhere for StandIn {
    fn reaches_hub(&self) -> bool {
        false
    }

    fn coordinator_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn pass_up_activity<'a>(
        &'a self,
        _: SpaceId,
        _: &'a SpaceRef,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Coordinators for StandIn {
    fn tell<'a>(&'a self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
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
