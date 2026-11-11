//! The removal of a workspace's worktree when the workspace becomes a chat:
//! it takes the path every removal takes, and commits the chat in place of
//! the workspace's record instead of deleting it.
use super::super::Manager;
use super::Removal;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{approvals::ApprovalRequest, conversions::record::ConvertedChat},
    execution::recovery::{self, LastSession, LastSessionExt},
    model::Workspace,
    removal::RemovalResult,
};
use anyhow::Result;

impl Manager {
    /// Remove the worktree of the workspace `id` as `shoal rm` does, once a
    /// checkpoint keeps its HEAD and unsaved files, keeping its branch and
    /// the spaces below it, and make it the stopped `chat`.
    pub(crate) async fn convert_worktree_to_chat(
        &self,
        id: &SpaceId,
        chat: &ConvertedChat,
    ) -> Result<RemovalResult> {
        self.remove(&SpaceSelector::id(id), Removal::Converted { chat })
            .await
    }

    /// Replace the workspace `id`, whose worktree is gone, with the stopped
    /// `chat`, returning the requests it decided, given back to the user.
    pub(super) async fn commit_conversion(
        &self,
        id: &SpaceId,
        chat: &ConvertedChat,
    ) -> Result<Vec<ApprovalRequest>> {
        let (id, chat) = (id.to_owned(), chat.clone());
        self.store
            .write(move |tx| crate::daemon::conversions::record::replace_workspace(tx, &id, &chat))
            .await
    }

    /// Forget the stopped agent of `workspace` that carries on as `chat`, so
    /// the removal record, which a restore reopens, leaves it out.
    pub(super) fn forget_converted_agent(
        &self,
        workspace: &Workspace,
        chat: &ConvertedChat,
    ) -> Result<()> {
        let record = recovery::record_path(&self.paths, &workspace.id, &chat.execution);
        recovery::consume(&record)?;
        recovery::consume(&LastSession::path(
            &self.paths,
            &workspace.id,
            &chat.execution,
        ))
    }
}
