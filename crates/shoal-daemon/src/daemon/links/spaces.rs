//! The registered repositories and the spaces requests about items come
//! from, and the watches of their linked items, as links reach them.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_links::{
    daemon::{SpaceAgent, Spaces, Watches},
    space::LinkSpace,
};

use crate::{
    agent::Agent,
    config::Effective,
    daemon::{manager::Manager, scope::Caller},
    forge::{
        items::{
            link::{ItemKind, Selection},
            reference::ItemRef,
        },
        numbers::PullNumber,
    },
    model::{ConflictCheck, Repository, Selected, Space, Workspace},
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

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

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(Manager::space_settings(self, space))
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(Manager::repository_settings(self, id))
    }

    fn settings_in<'a>(&'a self, id: Option<&'a RepositoryId>) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(Manager::space_settings_in(self, id))
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(Manager::workspace_settings(self, workspace))
    }

    fn space_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>> {
        Box::pin(Manager::issue_agent(self, id))
    }

    fn wake_cleanup(&self) {
        self.signals.wake_cleanup();
    }

    fn registered_named<'a>(
        &'a self,
        named: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(Manager::registered_named(self, named))
    }

    fn tracked_agents(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        Box::pin(async { self.running_agent_workspaces().await })
    }

    fn repository_workspaces<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(Manager::repository_workspaces(self, id))
    }

    fn reached_workspaces<'a>(
        &'a self,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async move {
            let root = self.reach_root(Some(caller)).await?;
            self.list_workspaces_within(root.as_ref()).await
        })
    }

    fn reaches<'a>(&'a self, caller: &'a Caller, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::reaches(self, caller, id))
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace(self, selector))
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace_by_id(self, id))
    }

    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(Manager::find_workspace(self, id))
    }

    fn verify_worktree<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::verify_worktree(self, workspace))
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(Manager::repositories(self))
    }

    fn branch_owner<'a>(
        &'a self,
        id: &'a RepositoryId,
        branch: &'a BranchName,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(Manager::branch_owner_in(self, id, branch))
    }

    fn pull_workspace<'a>(
        &'a self,
        id: &'a RepositoryId,
        url: &'a ItemRef,
        number: PullNumber,
    ) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(Manager::pull_workspace(self, id, url, number))
    }

    fn proposing_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<Agent>> {
        Box::pin(async move { self.agent_name(caller.execution_id()?).await })
    }

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(Manager::present_spaces(self))
    }

    fn watched_conflicts<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        only_sensitive: bool,
    ) -> BoxFuture<'a, Result<Option<ConflictCheck>>> {
        Box::pin(Manager::watched_conflicts(self, selector, only_sensitive))
    }
}

impl Watches for Manager {
    fn item_updates_queued(&self) {
        self.signals.item_updates_queued();
    }

    fn watch_item_updates(&self) -> tokio::sync::watch::Receiver<u64> {
        self.signals.watch_item_updates()
    }

    fn agent_changed(&self) {
        self.signals.agent_changed();
    }

    // Other workspaces' conflict checks read the default branch.
    fn item_merged(&self, id: &RepositoryId) {
        self.syncs.request(id);
    }

    // A PR's progress section follows its draft state.
    fn draft_changed(&self, id: &SpaceId) {
        self.publications.request(id);
    }
}
