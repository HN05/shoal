//! Removal policy: who removes a workspace and what that removal checks at
//! each stage, and what retains a workspace from every unattended removal.
use super::super::{done::CompletedAncestor, executions::StopPolicy};
use crate::client::space_ids::SpaceId;
use crate::daemon::space_processes::WorktreeTies;
use crate::refusal::{Reason, Refusal};
use crate::{
    daemon::{
        cleanup::{Rule, Snapshot},
        events::EventCause,
    },
    model::CleanupReason,
    removal::{InspectionPolicy, NOTHING, RemovalCheck, UnsavedWork},
};
use anyhow::{Result, ensure};

/// What keeps a workspace from every unattended removal. Sweeps report it
/// as a cleanup reason, and removal refuses with it at every stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    Held,
    Question,
    /// A PR proposal waits for the user's approval.
    PrProposal,
    /// A push waits for the user's approval.
    PushRequest,
    /// An agent launch in the workspace waits in the agent queue or is starting.
    QueuedAgent,
    /// A chat runs in the workspace's worktree.
    ChatRunning,
}

impl From<Retention> for CleanupReason {
    fn from(retention: Retention) -> Self {
        match retention {
            Retention::Held => Self::Held,
            Retention::Question => Self::Question,
            Retention::PrProposal => Self::PrProposal,
            Retention::PushRequest => Self::PushRequest,
            Retention::QueuedAgent => Self::QueuedAgent,
            Retention::ChatRunning => Self::ChatRunning,
        }
    }
}

/// Why removal refuses.
impl std::fmt::Display for Retention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Held => "workspace is held",
            Self::Question => "a question waits for the user",
            Self::PrProposal => "a PR proposal waits for approval",
            Self::PushRequest => "a push waits for approval",
            Self::QueuedAgent => "an agent launch in it waits in the agent queue",
            Self::ChatRunning => "a chat runs in it",
        })
    }
}

/// Who is removing the workspace, which decides how strict the checks are.
#[derive(Clone, Copy)]
pub(super) enum Removal<'a> {
    /// Explicit merge completion stops tracked agents without waiting for idle.
    Merged { head: &'a str },
    /// An assignment finished; all commits must already be kept, unless
    /// another branch has them under other commits.
    Completed {
        head: &'a str,
        integrated: bool,
        cause: EventCause,
    },
    /// A user request; work no branch or remote keeps needs their consent.
    /// A workspace process's request also leaves the user's holds in place.
    Manual {
        unsaved: &'a UnsavedWork,
        inspection: InspectionPolicy,
        release_holds: bool,
        /// The IDs of the only spaces it may take first, when the caller
        /// narrows what `unsaved` confirms.
        taking: Option<&'a [SpaceId]>,
    },
    /// Idle or disk-space cleanup; only safe, unchanged workspaces may go.
    Automatic {
        snapshot: Snapshot,
        cause: EventCause,
    },
    /// The directory was deleted outside Shoal; forget the workspace and
    /// release what it owned, retaining its branch.
    Deleted,
    /// An ancestor finished its assignment with cleanup, which takes the
    /// workspace whatever its own work: it goes with its unsaved files once
    /// a checkpoint keeps them, while the ancestor's completion stands.
    UnderCompleted { ancestor: &'a CompletedAncestor },
    /// The workspace's parent is being removed, which nothing about the
    /// workspace keeps: it goes with its unsaved files once a checkpoint
    /// keeps them. `cause`, `refuse_holds` and the `confirmed` IDs of the
    /// workspaces it may take below it are the parent removal's.
    Parent {
        cause: EventCause,
        refuse_holds: bool,
        confirmed: Option<&'a [SpaceId]>,
    },
}

impl<'a> Removal<'a> {
    pub(super) fn cause(self) -> EventCause {
        match self {
            Self::Manual { .. } => EventCause::Manual,
            Self::Automatic { cause, .. } => cause,
            Self::Merged { .. } => EventCause::Pr,
            Self::Completed { cause, .. } => cause,
            Self::Deleted => EventCause::MissingDirectory,
            Self::Parent { .. } => EventCause::Parent,
            Self::UnderCompleted { .. } => EventCause::ParentCompletion,
        }
    }

    pub(super) fn unsaved(self) -> &'a UnsavedWork {
        match self {
            Removal::Manual { unsaved, .. } => unsaved,
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Parent { .. }
            | Removal::UnderCompleted { .. }
            | Removal::Merged { .. }
            | Removal::Completed { .. } => &NOTHING,
        }
    }

    pub(super) fn inspection(self) -> InspectionPolicy {
        match self {
            Removal::Manual { inspection, .. } => inspection,
            Removal::Parent { .. } | Removal::UnderCompleted { .. } => InspectionPolicy::GitOnly,
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Merged { .. }
            | Removal::Completed { .. } => InspectionPolicy::IncludeDirectoryProcesses,
        }
    }

    pub(super) fn stop_policy(self) -> StopPolicy {
        match self {
            Removal::Manual { .. }
            | Removal::Merged { .. }
            | Removal::Completed { .. }
            | Removal::UnderCompleted { .. }
            | Removal::Parent { .. } => StopPolicy::ForRemoval,
            Removal::Automatic { .. } | Removal::Deleted => StopPolicy::RequireCompleteProof,
        }
    }

    /// What the worktree ties to the workspace for removal to stop.
    /// Automatic removal takes only unused workspaces, so a process working
    /// in one is activity that retains it, while a program built there and
    /// left running elsewhere is not; the others stop both.
    pub(super) fn worktree_ties(self) -> WorktreeTies {
        match self {
            Removal::Automatic { .. } => WorktreeTies::Programs,
            _ => WorktreeTies::All,
        }
    }

    /// Merged cleanup, an ancestor's completion and a parent's removal
    /// remove unsaved files once a checkpoint keeps them.
    pub(super) fn discards_archived_files(self) -> bool {
        matches!(
            self,
            Removal::Automatic {
                snapshot: Snapshot {
                    rule: Rule::Merged,
                    ..
                },
                ..
            } | Removal::Parent { .. }
                | Removal::UnderCompleted { .. }
        )
    }

    /// Unattended removals wait out the grace period, except to free disk
    /// space; a user's request does not.
    pub(super) fn waits_for_grace(self) -> bool {
        match self {
            Removal::Merged { .. } | Removal::Completed { .. } | Removal::UnderCompleted { .. } => {
                true
            }
            Removal::Automatic { cause, .. } => {
                !matches!(cause, EventCause::DiskSpace | EventCause::Manual)
            }
            Removal::Manual { .. } | Removal::Deleted | Removal::Parent { .. } => false,
        }
    }

    /// Unattended removals retain a workspace that is held or has an open
    /// question, whenever it was recorded; a user's request and its parent's
    /// removal override them.
    pub(super) fn unattended(self) -> bool {
        matches!(
            self,
            Removal::Automatic { .. }
                | Removal::Merged { .. }
                | Removal::Completed { .. }
                | Removal::UnderCompleted { .. }
        )
    }

    /// Whether the removal leaves alone the workspaces below that are in a
    /// stalled directory, as the cleanup sweep, which makes it, skips them.
    pub(super) fn skips_stalled(self) -> bool {
        self.unattended() || matches!(self, Removal::Deleted)
    }

    /// A workspace process's manual removal leaves the user's holds in
    /// place, below the workspace too.
    pub(super) fn refuses_holds(self) -> bool {
        match self {
            Removal::Manual { release_holds, .. } => !release_holds,
            Removal::Parent { refuse_holds, .. } => refuse_holds,
            _ => false,
        }
    }

    /// The removal of a child of the workspace this removes. A removal the
    /// user asked for is theirs, not the parent's, whatever its children.
    pub(super) fn of_child(self) -> Removal<'a> {
        Removal::Parent {
            cause: match self {
                Removal::Parent { cause, .. } => cause,
                removal => removal.cause(),
            },
            refuse_holds: self.refuses_holds(),
            confirmed: self.confirmed_children(),
        }
    }

    /// The IDs of the workspaces below the workspace that a manual removal
    /// confirmed, the only ones it takes; `None` takes whatever is below.
    pub(super) fn confirmed_children(self) -> Option<&'a [SpaceId]> {
        match self {
            Removal::Manual {
                unsaved, taking, ..
            } => taking.or(unsaved.children()),
            Removal::Parent { confirmed, .. } => confirmed,
            _ => None,
        }
    }

    /// Whether the user hears of the children it removes: those of any
    /// removal they did not ask for.
    pub(super) fn announces_children(self) -> bool {
        match self {
            Removal::Parent { cause, .. } => cause != EventCause::Manual,
            removal => removal.cause() != EventCause::Manual,
        }
    }

    /// Automatic removal needs a safe workspace; manual removal needs consent
    /// to delete unsaved work.
    pub(super) fn verify(self, check: &RemovalCheck, stage: Stage) -> Result<()> {
        match (self, stage) {
            (Removal::Completed { integrated, .. }, _) => {
                ensure!(
                    !check.dirty,
                    "workspace has uncommitted or untracked work; retaining it"
                );
                ensure!(
                    integrated || check.commits_kept(),
                    "workspace has commits neither pushed nor on the local default branch or the branch it builds on; retaining it"
                );
            }
            (Removal::Merged { .. }, _) => ensure!(
                !check.dirty,
                "workspace has uncommitted or untracked work; retaining it"
            ),
            (Removal::Automatic { snapshot, .. }, Stage::Initial) => ensure!(
                snapshot.rule.allows(check),
                "workspace is no longer {}",
                snapshot.rule.describe()
            ),
            (Removal::Automatic { snapshot, .. }, _) => ensure!(
                snapshot.rule.allows(check),
                "workspace changed {}",
                stage.change()
            ),
            (Removal::Manual { unsaved, .. }, Stage::Initial) => ensure!(
                unsaved.uncovered(check).is_empty(),
                Refusal::new(
                    Reason::UnsavedWork,
                    format!(
                        "removal would delete {}; confirm it or pass --yes",
                        unsaved.uncovered(check).join(" and ")
                    )
                )
            ),
            (Removal::Manual { unsaved, .. }, _) => ensure!(
                unsaved.uncovered(check).is_empty(),
                Refusal::new(
                    Reason::UnsavedWork,
                    format!(
                        "workspace changed {}; removal would delete {} as well",
                        stage.change(),
                        unsaved.uncovered(check).join(" and ")
                    )
                )
            ),
            (Removal::Deleted | Removal::Parent { .. } | Removal::UnderCompleted { .. }, _) => {}
        }
        Ok(())
    }
}

/// Recheck safety after operations that can change the worktree.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    Initial,
    AfterStop,
    /// Hooks run in the worktree.
    AfterHooks,
}

impl Stage {
    /// When a change that fails this stage's recheck happened.
    pub(super) fn change(self) -> &'static str {
        match self {
            Stage::Initial => "before removal",
            Stage::AfterStop => "while stopping commands",
            Stage::AfterHooks => "during removal hooks",
        }
    }
}
