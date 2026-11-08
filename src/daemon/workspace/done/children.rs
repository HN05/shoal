//! The spaces below a completed space: an ancestor's completion with
//! cleanup takes them whatever their own work, while it stands.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use super::super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::spaces::removal_notices::Cleanup,
    model::{Chat, SpaceKind, Workspace},
};

/// A space above a workspace whose completion with cleanup stands.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read once by the sweep that made it"
)]
pub(crate) enum CompletedAncestor {
    Workspace(Workspace),
    Chat(Chat),
}

impl CompletedAncestor {
    pub(crate) fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }
}

impl Manager {
    /// The nearest space above the space `id`, a workspace or a chat, whose
    /// completion with cleanup stands, which takes the space.
    pub(crate) async fn completed_ancestor(
        &self,
        id: &SpaceId,
    ) -> Result<Option<CompletedAncestor>> {
        for ancestor in self.ancestors(id).await? {
            let found = match ancestor.kind {
                SpaceKind::Workspace => {
                    (self.find_workspace(&ancestor.id).await?).map(CompletedAncestor::Workspace)
                }
                SpaceKind::Chat => {
                    (self.chat_by_id(&ancestor.id).await?).map(CompletedAncestor::Chat)
                }
            };
            // One removed meanwhile leaves the tree with what is below it.
            let Some(completed) = found else {
                return Ok(None);
            };
            if self.completion_stands(&completed).await? {
                return Ok(Some(completed));
            }
        }
        Ok(None)
    }

    /// Whether `ancestor` is marked done with cleanup, a chat as recorded
    /// now, a workspace at its current HEAD. A HEAD that cannot be read, as
    /// off its branch or in a directory whose file access stalled, is not
    /// that HEAD. Only a recorded stall counts: the candidate checks of a
    /// workspace below run this under [`Manager::check_access`] already,
    /// which a second limit would cut short, and branch observation, earlier
    /// in each sweep, records a stall of `workspace`'s directory.
    pub(in crate::daemon::workspace) async fn completion_stands(
        &self,
        ancestor: &CompletedAncestor,
    ) -> Result<bool> {
        let workspace = match ancestor {
            CompletedAncestor::Workspace(workspace) => workspace,
            CompletedAncestor::Chat(chat) => {
                let recorded = self.chat_by_id(&chat.id).await?;
                return Ok(recorded
                    .and_then(|chat| chat.completion)
                    .is_some_and(|completion| completion.cleanup));
            }
        };
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(false);
        };
        if self.file_access.stalled(&workspace.path).is_some() {
            return Ok(false);
        }
        Ok(completion.cleanup
            && crate::daemon::links::pr::current_head(workspace)
                .await
                .is_ok_and(|head| head == completion.head))
    }

    /// Remove `workspace` for its completed `ancestor` once its grace period
    /// ends, unless something keeps it or workspaces below it remain, which
    /// go first under the same completion.
    pub(super) async fn sweep_under_completed(
        &self,
        workspace: &Workspace,
        ancestor: &CompletedAncestor,
    ) -> Result<()> {
        if !workspace.children.is_empty() || self.keep_reason(&workspace.id).await?.is_some() {
            return Ok(());
        }
        let removal = self
            .remove_under_completed(&SpaceSelector::id(&workspace.id), ancestor)
            .await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        let cleanup = Cleanup::AncestorCompletion(ancestor.name());
        self.announce_removal(workspace, cleanup, &removal).await
    }
}
