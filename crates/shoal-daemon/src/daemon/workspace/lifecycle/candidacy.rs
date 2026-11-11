//! Cleanup candidacy of any space: what keeps it from cleanup, and the
//! snapshot of a candidate, which its worktree's stage completes where it
//! has one.
use super::super::{Manager, space_kinds::AgentState, space_kinds::SpaceAgent};
use super::Retention;
use crate::client::space_ids::SpaceId;
use crate::{
    daemon::cleanup::{Rule, Snapshot, Tree, signals},
    model::{CleanupReason, PresentSpace, Workspace},
    process,
    removal::IdleRestore,
};
use anyhow::Result;

impl Manager {
    /// What keeps the space `id` from every unattended removal, completion
    /// and PR cleanup included, read fresh on each call.
    pub(crate) async fn keep_reason(&self, id: &SpaceId) -> Result<Option<Retention>> {
        if self.has_holds(id).await? {
            return Ok(Some(Retention::Held));
        }
        if self.has_open_questions(id).await? {
            return Ok(Some(Retention::Question));
        }
        if self.has_pending_pull_proposal(id).await? {
            return Ok(Some(Retention::PrProposal));
        }
        if self.has_pending_push(id).await? {
            return Ok(Some(Retention::PushRequest));
        }
        if self.chat_runs_in(id).await? {
            return Ok(Some(Retention::ChatRunning));
        }
        if self.converting(id).await? {
            return Ok(Some(Retention::Converting));
        }
        let waits = self.has_queued_launch(id).await? || self.restore_waits(id).await?;
        Ok(waits.then_some(Retention::QueuedAgent))
    }

    /// Assignment state whose own cleanup path decides removal: a completed
    /// ancestor's or the space's own completion, or a PR or issue it
    /// watches.
    async fn assignment_reason(&self, id: &SpaceId) -> Result<Option<CleanupReason>> {
        if let Some(ancestor) = self.completed_ancestor(id).await? {
            return Ok(Some(CleanupReason::AncestorCompleted {
                workspace: ancestor.name,
            }));
        }
        if self.pr_registration(id).await?.is_some() {
            return Ok(Some(CleanupReason::PrWatch));
        }
        if let Some(completion) = self.completion(id).await? {
            return Ok(Some(CleanupReason::Completed {
                integrated: completion.integrated,
            }));
        }
        if self.issue_registration(id).await?.is_some() {
            return Ok(Some(CleanupReason::Issue));
        }
        Ok(None)
    }

    /// The agent of `space`, once what Shoal records about it leaves it to
    /// `rule`, or the first reason it does not: what keeps it, its agent
    /// running, and for idle cleanup, a saved agent, assignment state and a
    /// coordinator working there. `cancelled` are the restores a removal
    /// stopped.
    async fn recorded_candidate(
        &self,
        space: &PresentSpace,
        rule: Rule,
        cancelled: &[IdleRestore],
    ) -> Result<Result<SpaceAgent, CleanupReason>> {
        let id = space.id();
        if let Some(retention) = self.keep_reason(id).await? {
            return Ok(Err(retention.into()));
        }
        let agent = self.space_agent(space, cancelled).await?;
        match agent.state {
            AgentState::Running => return Ok(Err(CleanupReason::AgentRunning)),
            AgentState::Saved if rule == Rule::Idle => return Ok(Err(CleanupReason::Recovering)),
            AgentState::Saved | AgentState::Stopped => {}
        }
        if rule == Rule::Idle {
            if let Some(reason) = self.assignment_reason(id).await? {
                return Ok(Err(reason));
            }
            if self.coordinator_runs(id).await? {
                return Ok(Err(CleanupReason::ParentAgentRunning));
            }
        }
        Ok(Ok(agent))
    }

    /// The signals of `space` as an idle cleanup candidate, or the first
    /// reason it is not one: what Shoal records about it, then its
    /// worktree, whose processes are read from `users`.
    async fn idle_signals(
        &self,
        space: &PresentSpace,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Result<u64, CleanupReason>> {
        let agent = match self
            .recorded_candidate(space, Rule::Idle, cancelled)
            .await?
        {
            Ok(agent) => agent,
            Err(reason) => return Ok(Err(reason)),
        };
        let head = match space.worktree() {
            Some(workspace) => match self.idle_worktree(workspace, cancelled, users).await? {
                Ok(head) => Some(head),
                Err(reason) => return Ok(Err(reason)),
            },
            None => None,
        };
        Ok(Ok(self.signals_of(space, agent, head.as_deref())))
    }

    /// The signals of `space` as a merged cleanup candidate, or `None` when
    /// it is not one: what Shoal records about it leaves it to merged
    /// cleanup, which assignment state does not keep, and its worktree, whose
    /// processes are read from `users`, is merged into the default branch
    /// and unused.
    async fn merged_signals(
        &self,
        space: &PresentSpace,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<u64>> {
        let Ok(agent) = self
            .recorded_candidate(space, Rule::Merged, cancelled)
            .await?
        else {
            return Ok(None);
        };
        let head = match space.worktree() {
            Some(workspace) => match self.merged_worktree(workspace, cancelled, users).await? {
                Some(head) => Some(head),
                None => return Ok(None),
            },
            None => None,
        };
        Ok(Some(self.signals_of(space, agent, head.as_deref())))
    }

    /// What `space` records that cleanup compares on every pass: its
    /// activity, when its `agent` last stopped, and its worktree's `head`,
    /// so a restore and a new stop between two passes start over.
    fn signals_of(&self, space: &PresentSpace, agent: SpaceAgent, head: Option<&str>) -> u64 {
        signals(self.usage.activity(space.id()), agent.stopped_at, head)
    }

    /// The signals of `space` as a candidate of `rule`, or `None` when it is
    /// not one, with the processes in its worktree read from `users`.
    pub(crate) async fn candidate_signals(
        &self,
        space: &PresentSpace,
        rule: Rule,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<u64>> {
        match rule {
            Rule::Idle => Ok(self.idle_signals(space, cancelled, users).await?.ok()),
            Rule::Merged => self.merged_signals(space, cancelled, users).await,
        }
    }

    /// The full snapshot of `space` as a candidate of `rule`, or `None` when
    /// it is not one: its signals, and its worktree's files, which take a
    /// walk.
    pub(crate) async fn candidate_snapshot(
        &self,
        space: &PresentSpace,
        rule: Rule,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<Snapshot>> {
        let Some(signals) = self
            .candidate_signals(space, rule, cancelled, users)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(Snapshot {
            rule,
            signals,
            tree: self.tree_of(space).await?.fingerprint,
        }))
    }

    /// One walk of the files of `space`: its worktree's, or none.
    pub(crate) async fn tree_of(&self, space: &PresentSpace) -> Result<Tree> {
        match space.worktree() {
            Some(workspace) => self.walk_worktree(workspace).await,
            None => Ok(Tree::EMPTY),
        }
    }

    /// The snapshot of `space` as an idle cleanup candidate, or why it is
    /// not one, with the processes in its worktree read from `users`.
    pub(crate) async fn idle_candidacy(
        &self,
        space: &PresentSpace,
        users: &process::DirectoryUsers,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        let signals = match self.idle_signals(space, &[], users).await? {
            Ok(signals) => signals,
            Err(reason) => return Ok(Err(reason)),
        };
        Ok(Ok(Snapshot {
            rule: Rule::Idle,
            signals,
            tree: self.tree_of(space).await?.fingerprint,
        }))
    }

    /// The signals of `space` whose idle timer runs, or `None` when what
    /// Shoal records about it, or its worktree's commands, leases or a
    /// process `users` finds there, keeps it. A worktree's Git state and
    /// files are left to the full inspections when the timer starts and
    /// when it expires, which fail or differ after any change to them.
    pub(crate) async fn idle_timer_signals(
        &self,
        space: &PresentSpace,
        users: &process::DirectoryUsers,
    ) -> Result<Option<u64>> {
        let Ok(agent) = self.recorded_candidate(space, Rule::Idle, &[]).await? else {
            return Ok(None);
        };
        let head = match space.worktree() {
            Some(workspace) => match self.idle_timer_worktree(workspace, users).await? {
                Some(head) => Some(head),
                None => return Ok(None),
            },
            None => None,
        };
        Ok(Some(self.signals_of(space, agent, head.as_deref())))
    }

    /// When `space` was last used: the later of its recorded activity and
    /// its agent's last stop, or `None` while neither is recorded, as while
    /// a chat's agent runs.
    pub(crate) async fn last_used(
        &self,
        space: &PresentSpace,
    ) -> Result<Option<crate::time::UnixTime>> {
        let stopped_at = self.space_agent(space, &[]).await?.stopped_at;
        Ok(self.active_at(space.id()).await?.max(stopped_at))
    }

    /// Why automatic cleanup keeps the workspace, or its idle delay when
    /// nothing does; a failed inspection is reported as the reason.
    pub async fn cleanup_reason(&self, workspace: &Workspace) -> CleanupReason {
        self.inspect_cleanup(&PresentSpace::Workspace(workspace.clone()))
            .await
            .unwrap_or_else(|error| CleanupReason::InspectionFailed {
                error: format!("{error:#}"),
            })
    }

    async fn inspect_cleanup(&self, space: &PresentSpace) -> Result<CleanupReason> {
        if let Some(reason) = space.worktree().and_then(super::worktree::not_ready) {
            return Ok(reason);
        }
        let users = process::DirectoryUsers::default();
        if let Err(reason) = self.idle_signals(space, &[], &users).await? {
            return Ok(reason);
        }
        let auto = self
            .present_space_settings(space)
            .await?
            .cleanup
            .auto
            .clone();
        Ok(if auto.enabled {
            CleanupReason::Idle {
                minutes: auto.idle_minutes,
            }
        } else {
            CleanupReason::IdleDisabled
        })
    }
}
