//! Removal policy: who removes a space, what that removal checks at each
//! stage, and what keeps a space from every unattended removal.
use crate::daemon::BelowElsewhere;
use crate::model::EventCause;
use crate::refusal::{Reason, Refusal};
use crate::space_processes::WorktreeTies;
use crate::{
    cleanup::{Rule, Snapshot},
    model::CleanupReason,
    removal::{InspectionPolicy, NOTHING, RemovalCheck, UnsavedWork},
};
use crate::{completion_cleanup::CompletedAncestor, stops::StopPolicy};
use anyhow::{Result, ensure};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

/// Sweeps report it as a cleanup reason, and removal refuses with it at
/// every stage. A space that cannot have the item, such as a chat a hold,
/// never has the retention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    Held,
    /// A thread the space opened waits for its holder.
    WaitingThread,
    PrProposal,
    PushRequest,
    /// A launch in the agent queue or starting, or a chat's restore once the
    /// daemon starts agents again.
    QueuedAgent,
    /// A chat runs in the workspace's worktree.
    ChatRunning,
    /// A conversion's agent restarts in or from the workspace once its turn
    /// ends.
    Converting,
    /// The space looks for the default branch's CI runs for a merge it
    /// watched, or follows one that has not finished.
    MergeRuns,
}

impl From<Retention> for CleanupReason {
    fn from(retention: Retention) -> Self {
        match retention {
            Retention::Held => Self::Held,
            Retention::WaitingThread => Self::WaitingThread,
            Retention::PrProposal => Self::PrProposal,
            Retention::PushRequest => Self::PushRequest,
            Retention::QueuedAgent => Self::QueuedAgent,
            Retention::ChatRunning => Self::ChatRunning,
            Retention::Converting => Self::Converting,
            Retention::MergeRuns => Self::MergeRuns,
        }
    }
}

/// Why removal refuses.
impl std::fmt::Display for Retention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Held => "the space is held",
            Self::WaitingThread => "a thread it opened waits for its holder",
            Self::PrProposal => "a PR proposal waits for approval",
            Self::PushRequest => "a push waits for approval",
            Self::QueuedAgent => "its agent waits to start",
            Self::ChatRunning => "a chat runs in it",
            Self::Converting => "its agent restarts after a conversion",
            Self::MergeRuns => "it waits for CI runs of its merge",
        })
    }
}

/// Decides how strict the checks are.
#[derive(Clone, Copy)]
pub enum Removal<'a> {
    /// Stops tracked agents without waiting for them to go idle.
    Merged { head: &'a str },
    /// All commits must already be kept, unless another branch has them
    /// under other commits.
    Completed {
        head: &'a str,
        integrated: bool,
        cause: EventCause,
    },
    /// Work no branch or remote keeps needs the user's consent. A workspace
    /// process's request also leaves the user's holds in place.
    Manual {
        unsaved: &'a UnsavedWork,
        inspection: InspectionPolicy,
        release_holds: bool,
        /// The only spaces it may take first, when the caller narrows what
        /// `unsaved` confirms.
        taking: Option<&'a [SpaceId]>,
        /// The space goes with its repository. That removal holds the Git
        /// gate, under which no creation or setup starts, and waited for the
        /// creations under way to end.
        for_repository: bool,
        /// By workspace, when the removal ends a space another machine
        /// imported.
        moved: Option<&'a BTreeMap<SpaceId, CheckpointNumber>>,
        /// It ends the space here only. The space lives on on another
        /// machine a move or a takeover gave it, so what other machines keep
        /// of it stays.
        lives_on: bool,
    },
    /// Idle, merged or disk-space cleanup, or the user's request to clean
    /// up. Only safe, unchanged spaces may go.
    Automatic {
        snapshot: Snapshot,
        cause: EventCause,
    },
    /// The directory was deleted outside Shoal. Forgets the workspace and
    /// releases what it owned, keeping its branch.
    Deleted,
    /// An ancestor's completion with cleanup takes the space whatever its own
    /// work, while it stands. A worktree goes with its unsaved files once a
    /// checkpoint keeps them.
    UnderCompleted { ancestor: &'a CompletedAncestor },
    /// Nothing about the space keeps it. A worktree goes with its unsaved
    /// files once a checkpoint keeps them. The fields are the parent
    /// removal's.
    Parent {
        cause: EventCause,
        refuse_holds: bool,
        confirmed: Option<&'a [SpaceId]>,
        moved: Option<&'a BTreeMap<SpaceId, CheckpointNumber>>,
        lives_on: bool,
    },
    /// The worktree goes once a checkpoint keeps its unsaved files. Its
    /// branch, its record under the chat's kind and the spaces below it
    /// stay. The user's holds refuse it.
    Converted {
        chat: &'a crate::conversions::record::ConvertedChat,
    },
}

impl<'a> Removal<'a> {
    /// A conversion keeps the space, and so does the end here of one that
    /// lives on on another machine.
    pub fn ends_space(self) -> bool {
        !self.converts() && !self.lives_on()
    }

    fn lives_on(self) -> bool {
        match self {
            Removal::Manual { lives_on, .. } | Removal::Parent { lives_on, .. } => lives_on,
            _ => false,
        }
    }

    /// As of the children here. None for a conversion, which keeps them.
    pub fn below_elsewhere(self) -> Option<BelowElsewhere<'a>> {
        (!self.converts()).then(|| BelowElsewhere {
            refuse_holds: self.refuses_holds(),
            confirmed: self.confirmed_children(),
            cause: self.root_cause(),
        })
    }

    pub(super) fn cause(self) -> EventCause {
        match self {
            Self::Manual {
                for_repository: true,
                ..
            } => EventCause::Repository,
            Self::Manual { .. } => EventCause::Manual,
            Self::Automatic { cause, .. } => cause,
            Self::Merged { .. } => EventCause::Pr,
            Self::Completed { cause, .. } => cause,
            Self::Deleted => EventCause::MissingDirectory,
            // A space below one its repository's removal takes goes with the
            // repository too.
            Self::Parent {
                cause: EventCause::Repository,
                ..
            } => EventCause::Repository,
            Self::Parent { .. } => EventCause::Parent,
            Self::UnderCompleted { .. } => EventCause::ParentCompletion,
            Self::Converted { .. } => EventCause::Manual,
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
            | Removal::Completed { .. }
            | Removal::Converted { .. } => &NOTHING,
        }
    }

    pub fn inspection(self) -> InspectionPolicy {
        match self {
            Removal::Manual { inspection, .. } => inspection,
            Removal::Parent { .. } | Removal::UnderCompleted { .. } | Removal::Converted { .. } => {
                InspectionPolicy::GitOnly
            }
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Merged { .. }
            | Removal::Completed { .. } => InspectionPolicy::IncludeDirectoryProcesses,
        }
    }

    pub fn stop_policy(self) -> StopPolicy {
        match self {
            Removal::Manual { .. }
            | Removal::Merged { .. }
            | Removal::Completed { .. }
            | Removal::UnderCompleted { .. }
            | Removal::Parent { .. }
            | Removal::Converted { .. } => StopPolicy::ForRemoval,
            Removal::Automatic { .. } | Removal::Deleted => StopPolicy::RequireCompleteProof,
        }
    }

    /// Automatic removal takes only unused workspaces, so a process working
    /// in one is activity that retains it, while a program built there and
    /// left running elsewhere is not. The others stop both.
    pub(super) fn worktree_ties(self) -> WorktreeTies {
        match self {
            Removal::Automatic { .. } => WorktreeTies::Programs,
            _ => WorktreeTies::All,
        }
    }

    /// Merged cleanup, an ancestor's completion, a parent's removal and a
    /// conversion remove unsaved files once a checkpoint keeps them.
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
                | Removal::Converted { .. }
        )
    }

    /// Unattended removals wait out the grace period, except to free disk
    /// space. A user's request does not.
    pub(super) fn waits_for_grace(self) -> bool {
        match self {
            Removal::Merged { .. } | Removal::Completed { .. } | Removal::UnderCompleted { .. } => {
                true
            }
            Removal::Automatic { cause, .. } => {
                !matches!(cause, EventCause::DiskSpace | EventCause::Manual)
            }
            Removal::Manual { .. }
            | Removal::Deleted
            | Removal::Parent { .. }
            | Removal::Converted { .. } => false,
        }
    }

    /// Unattended removals retain a space that is held or has a thread that
    /// waits, whenever it was recorded. A user's request and its parent's
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

    /// Keeps the workspace's record, under a chat's kind, and the spaces
    /// below it, taking only the chats in its worktree.
    pub(super) fn converts(self) -> bool {
        matches!(self, Removal::Converted { .. })
    }

    /// The cleanup sweep, which makes such removals, skips spaces in a
    /// stalled directory.
    pub(super) fn skips_stalled(self) -> bool {
        self.unattended() || matches!(self, Removal::Deleted)
    }

    /// A workspace process's manual removal leaves the user's holds in
    /// place, below the space too.
    pub fn refuses_holds(self) -> bool {
        match self {
            Removal::Manual { release_holds, .. } => !release_holds,
            Removal::Parent { refuse_holds, .. } => refuse_holds,
            Removal::Converted { .. } => true,
            _ => false,
        }
    }

    /// A removal the user asked for is theirs, not the parent's, whatever its
    /// children.
    pub fn of_child(self) -> Removal<'a> {
        Removal::Parent {
            cause: match self {
                Removal::Parent { cause, .. } => cause,
                removal => removal.cause(),
            },
            refuse_holds: self.refuses_holds(),
            confirmed: self.confirmed_children(),
            moved: self.moved(),
            lives_on: self.lives_on(),
        }
    }

    /// By workspace. The last check refuses a workspace that no longer has
    /// what its own keeps, which only the other machine has.
    pub(super) fn moved(self) -> Option<&'a BTreeMap<SpaceId, CheckpointNumber>> {
        match self {
            Removal::Manual { moved, .. } | Removal::Parent { moved, .. } => moved,
            _ => None,
        }
    }

    /// The only ones a manual removal takes. `None` takes whatever is below.
    pub(super) fn confirmed_children(self) -> Option<&'a [SpaceId]> {
        match self {
            Removal::Manual {
                unsaved, taking, ..
            } => taking.or(unsaved.children()),
            Removal::Parent { confirmed, .. } => confirmed,
            _ => None,
        }
    }

    /// The user hears of the children of any removal they did not ask for,
    /// themselves or through the space's repository.
    pub(super) fn announces_children(self) -> bool {
        !matches!(
            self.root_cause(),
            EventCause::Manual | EventCause::Repository
        )
    }

    /// Its own, or for a child's, the cause of the removal of the space
    /// above it.
    fn root_cause(self) -> EventCause {
        match self {
            Removal::Parent { cause, .. } => cause,
            removal => removal.cause(),
        }
    }

    /// Automatic removal needs a safe worktree, manual removal consent to
    /// delete unsaved work.
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
            (
                Removal::Deleted
                | Removal::Parent { .. }
                | Removal::UnderCompleted { .. }
                | Removal::Converted { .. },
                _,
            ) => {}
        }
        Ok(())
    }
}

/// Removal rechecks at each stage, after operations that can change the
/// space.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    Initial,
    AfterStop,
    /// Once the spaces below are gone and, in a worktree, its removal hooks
    /// ran, just before the space goes.
    Final,
}

impl Stage {
    pub(super) fn change(self) -> &'static str {
        match self {
            Stage::Initial => "before removal",
            Stage::AfterStop => "while stopping commands",
            Stage::Final => "during removal hooks",
        }
    }
}
