//! The spaces below a completed space: an ancestor's completion with
//! cleanup takes them whatever their own work, while it stands.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use crate::model::Space;

/// A space above another whose completion with cleanup stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedAncestor {
    pub id: SpaceId,
    pub name: SpaceName,
}

impl Spaces {
    /// The nearest space above the space `id` whose completion with cleanup
    /// stands, which takes the space.
    pub async fn completed_ancestor(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Option<CompletedAncestor>> {
        for ancestor in self.ancestors(id).await? {
            // One removed meanwhile leaves the tree with what is below it.
            let Some(space) = self.find_space(&ancestor.id).await? else {
                return Ok(None);
            };
            if self.completion_stands(daemon, &space).await? {
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
    /// is not that HEAD; the read runs under `FileAccess::checked`, so a
    /// stall holds it no longer than file access may take.
    pub async fn completion_stands(&self, daemon: &dyn Daemon, space: &Space) -> Result<bool> {
        let Some(completion) = self.completion(&space.id).await? else {
            return Ok(false);
        };
        if !completion.cleanup {
            return Ok(false);
        }
        let Some(workspace) = space.to_workspace() else {
            return Ok(true);
        };
        if self.file_access.stalled(&workspace.path).is_some() {
            return Ok(false);
        }
        let head = daemon.current_head(&workspace);
        Ok((self
            .file_access
            .checked(&self.notifications, &workspace.path, head)
            .await)
            .is_ok_and(|head| head == completion.head))
    }
}
