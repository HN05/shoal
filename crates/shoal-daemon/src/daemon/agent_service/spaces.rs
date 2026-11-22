//! The spaces agents run in.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Spaces;

use crate::agent::Agent;
use crate::config::Effective;
use crate::config::modes::ModeName;
use crate::daemon::manager::Manager;
use crate::daemon::scope::Caller;
use crate::hooks::HookKind;
use crate::model::Chat;
use crate::model::Completion;
use crate::model::QueueEntry;
use crate::model::QueueRequest;
use crate::model::Repository;
use crate::model::Space;
use crate::model::Workspace;
use crate::model::WorkspaceUsage;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::config::permissions::PermissionChange;
use shoal_protocol::config::permissions::PermissionsConfig;
use shoal_protocol::progress::Progress;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;
use std::path::PathBuf;

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

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(self.present_spaces())
    }

    fn visible_spaces<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(self.visible_spaces(caller))
    }

    fn workspace_below<'a>(
        &'a self,
        caller: &'a Caller,
        space: &'a mut SpaceSelector,
        refusal: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.workspace_below(caller, space, refusal).await?;
            Ok(())
        })
    }

    fn reachable_queue<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(self.reachable_queue(caller))
    }

    fn agent_of<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>> {
        Box::pin(self.agent_of(id))
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings(self, space))
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.workspace_settings(self, workspace))
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_with_templates(self, workspace),
        )
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(self.workspace(selector))
    }

    fn workspace_paths(&self) -> BoxFuture<'_, Result<Vec<PathBuf>>> {
        Box::pin(async {
            let workspaces = self.list_workspaces().await?;
            Ok((workspaces.into_iter())
                .map(|workspace| workspace.part.path)
                .collect())
        })
    }

    fn chat_workspace<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(self.chat_workspace(selector))
    }

    fn chat_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Chat>>> {
        Box::pin(self.chat_by_id(id))
    }

    fn chat_repository<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(self.chat_repository(chat))
    }

    fn chat_checkout_path<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(self.chat_checkout_path(chat))
    }

    fn chat_tmp_dir(
        &self,
        repository: Option<&Repository>,
        chat: &Chat,
    ) -> Result<Option<PathBuf>> {
        self.chat_tmp_dir(repository, chat)
    }

    fn completion<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Completion>>> {
        Box::pin(self.completion(id))
    }

    fn reopen_for_resume<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.reopen_for_resume(id))
    }

    fn space_blocked<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.space_blocked(id))
    }

    fn space_progress<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Progress>>> {
        Box::pin(self.space_progress(selector))
    }

    fn launch_priority<'a>(
        &'a self,
        request: &'a QueueRequest,
        repository: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<Priority>>> {
        Box::pin(self.launch_priority(request, repository))
    }

    fn ensure_change_held<'a>(
        &'a self,
        giver: Option<&'a Caller>,
        space: &'a Space,
        change: &'a PermissionChange,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.ensure_change_held(giver, space, change, repository))
    }

    fn ensure_agent_held<'a>(
        &'a self,
        giver: Option<&'a Caller>,
        space: &'a SpaceSelector,
        mode: &'a ModeName,
        given: &'a PermissionsConfig,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.ensure_agent_held(giver, space, mode, given))
    }

    fn verify_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.verify_space(space))
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.touch(id))
    }

    fn run_agent_exit_hook<'a>(
        &'a self,
        space: &'a Space,
        agent: &'a Agent,
        exit_code: Option<i32>,
        complete: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.run_agent_exit_hook(space, agent, exit_code, complete))
    }

    fn workspace_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(self.workspace_hook(workspace, kind))
    }

    fn checkpoint_after_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.checkpoint_after_agent(id))
    }

    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage> {
        self.workspace_usage(id)
    }
}
