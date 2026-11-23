//! The removal of a workspace's worktree when the workspace becomes a chat:
//! it takes the path every removal takes, and commits the chat in place of
//! the workspace's record instead of deleting it.
use super::Removal;
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    removal::RemovalResult,
    {approvals::ApprovalRequest, conversions::record::ConvertedChat},
};
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

impl Spaces {
    /// Remove the worktree of the workspace `id` as `shoal rm` does, once a
    /// checkpoint keeps its HEAD and unsaved files, keeping its branch and
    /// the spaces below it, and make it the stopped `chat`.
    pub async fn convert_worktree_to_chat(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        chat: &ConvertedChat,
    ) -> Result<RemovalResult> {
        (self
            .remove_space(daemon, id, Removal::Converted { chat })
            .await?)
            .workspace_result()
    }

    /// Replace the workspace `id`, whose worktree is gone, with the stopped
    /// `chat`, giving the workspace's removal record an ID of its own, since
    /// the chat keeps this one, and returning the requests it decided, given
    /// back to the user.
    pub(super) async fn commit_conversion(
        &self,
        id: &SpaceId,
        chat: &ConvertedChat,
    ) -> Result<Vec<ApprovalRequest>> {
        let (id, chat) = (id.to_owned(), chat.clone());
        self.store
            .write(move |tx| {
                super::super::restore::rekey_converted(tx, &id)?;
                crate::conversions::record::replace_workspace(tx, &id, &chat)
            })
            .await
    }
}
