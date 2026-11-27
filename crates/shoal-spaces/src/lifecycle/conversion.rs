//! Removing a workspace's worktree when the workspace becomes a chat: the
//! path every removal takes, committing the chat in place of the
//! workspace's record instead of deleting it.
use super::Removal;
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    removal::RemovalResult,
    {approvals::ApprovalRequest, conversions::pending::ConvertedChat},
};
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

impl Spaces {
    /// Removes as `shoal rm` does, once a checkpoint keeps HEAD and unsaved
    /// files, keeping the branch and the spaces below.
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

    /// The workspace's removal record gets an ID of its own, since the chat
    /// keeps this one. Returns the requests it decided, given back to the
    /// user.
    pub(super) async fn commit_conversion(
        &self,
        id: &SpaceId,
        chat: &ConvertedChat,
    ) -> Result<Vec<ApprovalRequest>> {
        let (id, chat, records) = (id.to_owned(), chat.clone(), self.records.clone());
        self.store
            .write(move |tx| {
                super::super::restore::rekey_converted(tx, &id)?;
                crate::conversions::record::replace_workspace(tx, &*records, &id, &chat)
            })
            .await
    }
}
