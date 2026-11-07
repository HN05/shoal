//! Announcing a space's removal: the notice that a cleanup removed it, or
//! the notice and journal record that it retained it.
use anyhow::Result;

use crate::{
    daemon::{
        events::EventCause,
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    model::{Chat, SpaceKind, SpaceRef, Workspace},
};

/// A space a cleanup removed or retained.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Announced<'a> {
    kind: SpaceKind,
    id: &'a str,
    name: &'a str,
}

impl<'a> From<&'a Workspace> for Announced<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self {
            kind: SpaceKind::Workspace,
            id: &workspace.id,
            name: &workspace.name,
        }
    }
}

impl<'a> From<&'a Chat> for Announced<'a> {
    fn from(chat: &'a Chat) -> Self {
        Self {
            kind: SpaceKind::Chat,
            id: &chat.id,
            name: &chat.name,
        }
    }
}

impl<'a> From<&'a SpaceRef> for Announced<'a> {
    fn from(space: &'a SpaceRef) -> Self {
        Self {
            kind: space.kind,
            id: &space.id,
            name: &space.name,
        }
    }
}

impl Announced<'_> {
    /// The notice that `cleanup` removed the space. A workspace's says how
    /// to restore it, unless the cleanup forgot it without a checkpoint.
    fn removal_notice(self, cleanup: Cleanup<'_>) -> String {
        let removal = cleanup.removal();
        match self.kind {
            SpaceKind::Chat => format!("chat {removal}"),
            SpaceKind::Workspace if matches!(cleanup, Cleanup::DeletedWorktree) => removal,
            SpaceKind::Workspace => {
                format!(
                    "{removal}; restore it with shoal work restore {}",
                    self.name
                )
            }
        }
    }
}

/// The cleanup that removed or retained a space.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cleanup<'a> {
    Idle,
    Merged,
    /// Disk space cleanup, with the state of the filesystem it frees.
    DiskSpace(&'a str),
    /// Cleanup after the space's own completion.
    Completion,
    /// Cleanup after the completion of the ancestor named.
    AncestorCompletion(&'a str),
    /// PR cleanup, after every watched PR merged or the user acknowledged
    /// the merge.
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
    /// The cleanup, as the subject of its notices.
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

    /// What the cleanup did to a space it removed.
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

    /// The cause the journal records for the workspace `id` the cleanup
    /// retained.
    async fn cause(self, manager: &Manager, id: &str) -> Result<EventCause> {
        Ok(match self {
            Self::Idle => EventCause::Idle,
            Self::Merged => EventCause::Merged,
            Self::DiskSpace(_) => EventCause::DiskSpace,
            Self::Completion => manager.completion_cause(id).await?,
            Self::AncestorCompletion(_) => EventCause::ParentCompletion,
            Self::Pr { .. } => EventCause::Pr,
            Self::DeletedWorktree => EventCause::MissingDirectory,
            Self::Parent(_) => EventCause::Parent,
        })
    }
}

impl Manager {
    /// Tell the user that `cleanup` removed `space`, or, when `removal`
    /// failed, that it retained it, which a workspace's journal records.
    /// The notice goes out even when recording fails, whose error is
    /// returned.
    pub(crate) async fn announce_removal<T>(
        &self,
        space: impl Into<Announced<'_>>,
        cleanup: Cleanup<'_>,
        removal: &Result<T>,
    ) -> Result<()> {
        let space = space.into();
        let about = About::space(space.name, space.id);
        let Err(error) = removal else {
            log!("{} removed {} {}", cleanup.name(), space.kind, space.name);
            let notice = space.removal_notice(cleanup);
            (self.notify_about(about, NotificationKind::SpaceRemoved, notice)).await;
            return Ok(());
        };
        log!(
            "{} retained {} {}: {error:#}",
            cleanup.name(),
            space.kind,
            space.name
        );
        let recorded = self.record_retention(space, cleanup, error).await;
        let notice = format!("{} retained the {}: {error:#}", cleanup.name(), space.kind);
        (self.notify_about(about, NotificationKind::CleanupFailed, notice)).await;
        recorded
    }

    /// Record in the journal that `cleanup` retained `space` with `error`.
    /// A chat's journal records no retention.
    async fn record_retention(
        &self,
        space: Announced<'_>,
        cleanup: Cleanup<'_>,
        error: &anyhow::Error,
    ) -> Result<()> {
        if space.kind == SpaceKind::Chat {
            return Ok(());
        }
        let cause = cleanup.cause(self, space.id).await?;
        self.record_retained(space.id, cause, error).await
    }
}

#[cfg(test)]
mod tests;
