//! Announcing that a cleanup removed or retained a space.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::Result;

use crate::{
    model::{Space, SpaceKind, SpaceRef, Workspace},
    {
        log,
        notifications::{About, AlertKind},
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

#[derive(Debug, Clone, Copy)]
pub struct Announced<'a> {
    pub kind: SpaceKind,
    pub id: &'a SpaceId,
    pub name: &'a SpaceName,
    /// Notices of a space without a worktree name its kind.
    pub worktree: bool,
}

impl<'a> From<&'a Workspace> for Announced<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self {
            kind: SpaceKind::Workspace,
            id: &workspace.id,
            name: &workspace.name,
            worktree: true,
        }
    }
}

impl<'a> From<&'a Space> for Announced<'a> {
    fn from(space: &'a Space) -> Self {
        Self {
            kind: space.kind(),
            id: &space.id,
            name: &space.name,
            worktree: space.worktree().is_some(),
        }
    }
}

impl<'a> From<&'a SpaceRef> for Announced<'a> {
    fn from(space: &'a SpaceRef) -> Self {
        Self {
            kind: space.kind(),
            id: &space.id,
            name: &space.name,
            worktree: space.kind().has_worktree(),
        }
    }
}

impl Announced<'_> {
    /// Says how to restore the space, unless the cleanup forgot it without a
    /// removal record.
    pub fn removal_notice(self, cleanup: Cleanup<'_>) -> String {
        let removal = match self.worktree {
            true => cleanup.removal(),
            false => format!("{} {}", self.kind, cleanup.removal()),
        };
        match cleanup {
            Cleanup::DeletedWorktree => removal,
            _ => format!("{removal}; restore it with shoal restore {}", self.id),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Cleanup<'a> {
    Idle,
    Merged,
    /// With the state of the filesystem it frees.
    DiskSpace(&'a str),
    Completion,
    /// Cleanup after the completion of the ancestor named.
    AncestorCompletion(&'a str),
    /// After every watched PR merged or the user acknowledged the merge.
    Pr {
        acknowledged: bool,
    },
    /// Forgetting a workspace whose directory was deleted.
    DeletedWorktree,
    /// The removal of the parent named, which takes the spaces below it
    /// first.
    Parent(&'a str),
}

impl Cleanup<'_> {
    /// As the subject of its notices.
    fn name(self) -> String {
        match self {
            Self::Idle => "idle cleanup".to_owned(),
            Self::Merged => "merged cleanup".to_owned(),
            Self::DiskSpace(_) => "disk space cleanup".to_owned(),
            Self::Completion => "completion cleanup".to_owned(),
            Self::AncestorCompletion(ancestor) => {
                format!("cleanup after {ancestor} was marked done")
            }
            Self::Pr { .. } => "PR cleanup".to_owned(),
            Self::DeletedWorktree => "deleted worktree cleanup".to_owned(),
            Self::Parent(parent) => format!("the removal of {parent}"),
        }
    }

    fn removal(self) -> String {
        match self {
            Self::Idle | Self::Merged => format!("removed by {}", self.name()),
            Self::DiskSpace(filesystem) => format!("removed by disk space cleanup; {filesystem}"),
            Self::Completion => "removed after the assignment was marked done".to_owned(),
            Self::AncestorCompletion(ancestor) => {
                format!("removed after {ancestor} was marked done")
            }
            Self::Pr {
                acknowledged: false,
            } => "removed after all watched pull requests merged".to_owned(),
            Self::Pr { acknowledged: true } => "removed after the merge acknowledgement".to_owned(),
            Self::DeletedWorktree => {
                "forgotten after its directory was deleted; branch retained".to_owned()
            }
            Self::Parent(parent) => format!("removed with {parent}"),
        }
    }

    async fn cause(self, daemon: &dyn Daemon, id: &SpaceId) -> Result<EventCause> {
        Ok(match self {
            Self::Idle => EventCause::Idle,
            Self::Merged => EventCause::Merged,
            Self::DiskSpace(_) => EventCause::DiskSpace,
            Self::Completion => daemon.spaces().completion_cause(id).await?,
            Self::AncestorCompletion(_) => EventCause::ParentCompletion,
            Self::Pr { .. } => EventCause::Pr,
            Self::DeletedWorktree => EventCause::MissingDirectory,
            Self::Parent(_) => EventCause::Parent,
        })
    }
}

impl Spaces {
    /// When `removal` failed, says it retained the space, which the journal
    /// records. The notice goes out even when recording fails, whose error
    /// is returned.
    pub async fn announce_removal<T>(
        &self,
        daemon: &dyn Daemon,
        space: impl Into<Announced<'_>>,
        cleanup: Cleanup<'_>,
        removal: &Result<T>,
    ) -> Result<()> {
        let space = space.into();
        let about = About::space(space.name, space.id);
        let Err(error) = removal else {
            log!("{} removed {} {}", cleanup.name(), space.kind, space.name);
            // A parent's removal, which may be the user's, names the spaces
            // below it.
            if !matches!(cleanup, Cleanup::Parent(_)) {
                self.cleanup_removals.push(space.name);
            }
            let notice = space.removal_notice(cleanup);
            (daemon
                .coordination()
                .notify_about(daemon, about, AlertKind::SpaceRemoved, notice))
            .await;
            return Ok(());
        };
        log!(
            "{} retained {} {}: {error:#}",
            cleanup.name(),
            space.kind,
            space.name
        );
        let recorded = self.record_retention(daemon, space, cleanup, error).await;
        let notice = format!("{} retained the {}: {error:#}", cleanup.name(), space.kind);
        (daemon
            .coordination()
            .notify_about(daemon, about, AlertKind::CleanupFailed, notice))
        .await;
        recorded
    }

    async fn record_retention(
        &self,
        daemon: &dyn Daemon,
        space: Announced<'_>,
        cleanup: Cleanup<'_>,
        error: &anyhow::Error,
    ) -> Result<()> {
        let cause = cleanup.cause(daemon, space.id).await?;
        crate::events::record_retained(&self.store, space.id, cause, error).await
    }
}

#[cfg(test)]
mod tests;
