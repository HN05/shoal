//! A stand-in for the rest of the daemon, which the links' tests build
//! their operations over: the issues and PRs a test assigns to each space,
//! and nothing else.
use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use shoal_core::config::Effective;
use shoal_daemon_base::{
    alerts::{About, Alert, AlertKind, NewAlert, Sent, Unread},
    callers::Caller,
    notifications::{Coordinators, Source},
    permitted_actions::Action,
};
use shoal_forge::ci::workflow::WorkflowLink;
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    approvals::ApprovalRequest,
    config::permissions::{self, PermissionKey},
    model::{ConflictCheck, Repository, Selected, Space, SpaceRef, Workspace},
    processes::Leftover,
    protocol::operations::OperationSpec,
    space_names::SpaceSelector,
};
use shoal_refs::CommitId;
use tokio::sync::watch;

use crate::{
    daemon::{
        Approvals, Elsewhere, Forges, Messages, Permissions, Reviews, SpaceAgent, Spaces, Watches,
    },
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

    fn repository_settings<'a>(&'a self, _: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn global_settings(&self) -> BoxFuture<'_, Result<Effective>> {
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

    fn repository_workspaces<'a>(
        &'a self,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn reached_workspaces<'a>(&'a self, _: &'a Caller) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn reaches<'a>(&'a self, _: &'a Caller, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn workspace<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn workspace_by_id<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn find_workspace<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(async { Ok(None) })
    }

    fn verify_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn caller_agent<'a>(&'a self, _: &'a Caller) -> BoxFuture<'a, Option<Agent>> {
        Box::pin(async { None })
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

impl Approvals for StandIn {
    fn announce<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ensure_gate_hears<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn tell_space<'a>(&'a self, _: &'a SpaceId, _: AlertKind, _: String) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn notify_about(&self, _: About, _: AlertKind, _: String) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn start_operation(
        &self,
        _: OperationSpec,
        _: Option<BTreeMap<String, String>>,
    ) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn operation_running(&self, _: &str) -> bool {
        false
    }
}

impl Reviews for StandIn {
    fn pr_review_required(&self, _: &Effective, _: Option<&Caller>) -> bool {
        false
    }

    fn require_pr_review<'a>(
        &'a self,
        _: &'a Workspace,
        _: &'a CommitId,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
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

    fn resolve_workflow<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(ForgeRepo, WorkflowLink)>> {
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

    fn caller_permissions<'a>(
        &'a self,
        _: &'a Caller,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<permissions::Permissions>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn authorize_actions<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: Vec<Action>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn refuse_denied<'a>(
        &'a self,
        _: &'a Caller,
        _: PermissionKey,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
