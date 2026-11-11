//! The spaces below a completed space: an ancestor's completion with
//! cleanup takes them whatever their own work, while it stands.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use anyhow::Result;

use super::super::Manager;
use crate::model::PresentSpace;

/// A space above another whose completion with cleanup stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedAncestor {
    pub(crate) id: SpaceId,
    pub(crate) name: SpaceName,
}

impl Manager {
    /// The nearest space above the space `id` whose completion with cleanup
    /// stands, which takes the space.
    pub(crate) async fn completed_ancestor(
        &self,
        id: &SpaceId,
    ) -> Result<Option<CompletedAncestor>> {
        for ancestor in self.ancestors(id).await? {
            // One removed meanwhile leaves the tree with what is below it.
            let Some(space) = self.find_space(&ancestor.id).await? else {
                return Ok(None);
            };
            if self.completion_stands(&space).await? {
                return Ok(Some(CompletedAncestor {
                    id: ancestor.id,
                    name: ancestor.name,
                }));
            }
        }
        Ok(None)
    }

    /// Whether `space` is marked done with cleanup, as recorded and, where it
    /// has a worktree, at the worktree's current HEAD. A HEAD that cannot be
    /// read, as off its branch or in a directory whose file access stalled,
    /// is not that HEAD. Only a recorded stall counts: branch observation,
    /// earlier in each sweep, records a stall of the worktree's directory.
    pub(in crate::daemon::workspace) async fn completion_stands(
        &self,
        space: &PresentSpace,
    ) -> Result<bool> {
        let Some(completion) = self.completion(space.id()).await? else {
            return Ok(false);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        let Some(workspace) = space.worktree() else {
            return Ok(true);
        };
        if self.file_access.stalled(&workspace.path).is_some() {
            return Ok(false);
        }
        Ok(crate::daemon::links::pr::current_head(workspace)
            .await
            .is_ok_and(|head| head == completion.head))
    }
}
