//! The named checks and views that confine a scoped caller to what its
//! scope allows.
use crate::client::repository_ids::RepositoryId;
use anyhow::{Context, Result, ensure};

use super::Caller;
use crate::{
    client::space_names::SpaceSelector,
    daemon::{
        approvals::ApprovalRequest, item_numbers::ItemNumber, questions::Question,
        workspace::Manager,
    },
    execution::recovery,
    model::{Chat, PresentSpace, QueueEntry, QueueRequest, Repository, RunningAgent, SpaceTarget},
    repository::Selector,
};

/// Why a scoped caller cannot start an agent regardless of the limits.
pub(in crate::daemon) const NOT_NOW: &str =
    "workspace processes start agents when a slot is free; leave out --now";

impl Manager {
    /// Put a space `caller` starts below the space `parent` names, which
    /// must be its own or one below it, else below its own space.
    pub(in crate::daemon) async fn parent_below(
        &self,
        caller: &Caller,
        parent: &mut Option<SpaceSelector>,
    ) -> Result<()> {
        let id = match parent.as_ref() {
            Some(named) => {
                let id = self.space(named).await?.id().to_owned();
                ensure!(
                    self.within_subtree(&caller.space_id, &id).await?,
                    "workspace processes start spaces only below their own space and those below it"
                );
                id
            }
            None => caller.space_id.clone(),
        };
        *parent = Some(SpaceSelector::id(&id));
        Ok(())
    }

    /// Confine a copy `caller` starts to the workspaces its scope reaches,
    /// live ones only, naming the copied one by ID from then on.
    pub(in crate::daemon) async fn copy_within(
        &self,
        caller: &Caller,
        source: &mut crate::protocol::CopySource,
    ) -> Result<()> {
        let workspace = self.workspace(&source.workspace).await.context(
            "workspace processes copy only existing workspaces; ask the user to start from a removed workspace's checkpoint",
        )?;
        ensure!(
            self.reaches(caller, &workspace.id).await?,
            "workspace processes copy only their own workspace and those below it"
        );
        source.workspace = SpaceSelector::id(&workspace.id);
        Ok(())
    }

    /// The repositories `caller` may see: its own space's, or all of them
    /// for an unscoped caller and a chat without a repository.
    pub(in crate::daemon) async fn visible_repositories(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Repository>> {
        let mut repositories = self.repositories().await?;
        if let Some(caller) = caller
            && let Some(own) = self.caller_repository(caller).await?
        {
            repositories.retain(|repository| repository.id == own);
        }
        Ok(repositories)
    }

    /// Whether the caller's own space is a chat.
    pub(in crate::daemon) async fn caller_is_chat(&self, caller: &Caller) -> Result<bool> {
        let id = caller.space_id.clone();
        let kind = (self.store)
            .run(move |db| crate::daemon::spaces::space_kind(db, &id))
            .await?;
        Ok(kind == Some(crate::model::SpaceKind::Chat))
    }

    /// The repository of the caller's own space; none for a chat without
    /// one.
    pub(in crate::daemon) async fn caller_repository(
        &self,
        caller: &Caller,
    ) -> Result<Option<RepositoryId>> {
        let space = self.space_by_id(&caller.space_id).await?;
        Ok(space.repository_id().cloned())
    }

    /// The spaces `target` finds among those `caller` may see: the ones its
    /// scope reaches and the workspace a chat runs in, or every space for
    /// an unscoped caller.
    pub(in crate::daemon) async fn find_visible_spaces(
        &self,
        target: &SpaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<PresentSpace>> {
        let mut within = self.reached_spaces(caller).await?;
        if let (Some(within), Some(caller)) = (&mut within, caller) {
            within.extend(self.chat_home(&caller.space_id).await?);
        }
        self.find_spaces(target, within).await
    }

    /// `space`, which must be the caller's own; `refusal` says why
    /// another is refused.
    pub(in crate::daemon) async fn own_workspace<'a>(
        &self,
        caller: &Caller,
        space: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        ensure!(
            self.space(space).await?.id() == &caller.space_id,
            "{refusal}"
        );
        Ok(space)
    }

    /// `space`, which must not be the caller's own; [`super::authorize`]
    /// confines it to the spaces below. `refusal` says why the caller's own
    /// is refused.
    pub(in crate::daemon) async fn workspace_below<'a>(
        &self,
        caller: &Caller,
        space: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        ensure!(
            self.space(space).await?.id() != &caller.space_id,
            "{refusal}"
        );
        Ok(space)
    }

    /// `workspace`, which must be one below the caller's whose agent stopped
    /// as `execution` the caller's process or one below it stopped, or whose
    /// restart by one of them left it stopped: an agent the user or Shoal
    /// stopped, or that exited on its own, is the user's to resume.
    pub(in crate::daemon) async fn stopped_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut SpaceSelector,
        execution: &str,
    ) -> Result<&'a mut SpaceSelector> {
        let workspace = self
            .workspace_below(
                caller,
                workspace,
                "scoped processes resume agents only in the workspaces below their own space",
            )
            .await?;
        let id = self.workspace(workspace).await?.id;
        let stopper = recovery::stopped_by(&self.paths, &id, execution)?;
        let ours = match stopper {
            Some(stopper) => self.within_subtree(&caller.space_id, &stopper).await?,
            None => false,
        };
        ensure!(
            ours,
            "scoped processes resume only the agents that they or the spaces below them stopped, with shoal stop or a restart; ask the user to resume this one"
        );
        Ok(workspace)
    }

    /// `chat`, which must be a stopped chat below the caller's space that
    /// the caller's process or one below it stopped: a chat the user or
    /// Shoal stopped, or whose agent exited, is the user's to resume.
    pub(in crate::daemon) async fn stopped_chat_below<'a>(
        &self,
        caller: &Caller,
        chat: &'a mut SpaceSelector,
    ) -> Result<&'a mut SpaceSelector> {
        let chat = self
            .workspace_below(
                caller,
                chat,
                "scoped processes resume only the chats below their own space",
            )
            .await?;
        let id = self.space(chat).await?.id().to_owned();
        let ours = match self.chat_stopped_by(&id).await? {
            Some(stopper) => self.within_subtree(&caller.space_id, &stopper).await?,
            None => false,
        };
        ensure!(
            ours,
            "scoped processes resume only the chats that they or the spaces below them stopped; ask the user to resume this one"
        );
        Ok(chat)
    }

    /// `workspace`, which must be one below the caller's that the user
    /// holds nothing of, nor of any workspace below it: removing it releases
    /// their holds, and only the holds agents placed are agents' to release.
    pub(in crate::daemon) async fn unheld_workspace_below<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        let workspace = self.workspace_below(caller, workspace, refusal).await?;
        let id = self.space(workspace).await?.id().to_owned();
        for id in self.subtree(&id).await? {
            // A chat below has no holds.
            let Some(held) = self.find_workspace(&id).await? else {
                continue;
            };
            ensure!(
                held.holds
                    .iter()
                    .all(|hold| hold.placed_by == crate::model::HoldPlacer::Agent),
                "{} has holds, which are the user's; ask the user to remove it",
                held.name
            );
        }
        Ok(workspace)
    }

    /// Approval request `number`, which must be routed to the caller to
    /// decide. Only the stored routing counts; the decision rechecks it with
    /// its grant.
    pub(in crate::daemon) async fn routed_approval(
        &self,
        caller: &Caller,
        number: ItemNumber,
    ) -> Result<ApprovalRequest> {
        let request = self.show_approval(number).await?;
        ensure!(
            request.decider.as_deref() == Some(caller.space_id.as_str()),
            "workspace processes decide only the approval requests routed to them; ask the user to run this command"
        );
        Ok(request)
    }

    /// The chats `caller` may see: those its scope reaches, or all of them
    /// for an unscoped caller.
    pub(in crate::daemon) async fn visible_chats(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Chat>> {
        let mut chats = self.chats().await?;
        if let Some(reached) = self.reached_spaces(caller).await? {
            chats.retain(|chat| reached.contains(&chat.id));
        }
        Ok(chats)
    }

    /// The `agents` `caller` may restart: those of the spaces its scope
    /// reaches but its own, or all of them for an unscoped caller.
    pub(in crate::daemon) async fn visible_agents(
        &self,
        caller: Option<&Caller>,
        mut agents: Vec<RunningAgent>,
    ) -> Result<Vec<RunningAgent>> {
        if let Some(caller) = caller {
            let reached = self.reached_spaces(Some(caller)).await?;
            agents.retain(|agent| {
                agent.space_id != caller.space_id
                    && reached
                        .as_ref()
                        .is_none_or(|reached| reached.contains(&agent.space_id))
            });
        }
        Ok(agents)
    }

    /// Refuse `caller` the restart of the workspace agent that runs, or ran
    /// before a restart, as `execution`, unless it runs in a workspace below
    /// the caller's space.
    pub(in crate::daemon) async fn ensure_agent_below(
        &self,
        caller: &Caller,
        execution: &str,
    ) -> Result<()> {
        let agents = self.running_agents().await?;
        let agents = self.visible_agents(Some(caller), agents).await?;
        ensure!(
            agents.iter().any(|agent| agent.ran_as(execution)),
            "scoped processes restart only the agents of the spaces below their own"
        );
        Ok(())
    }

    /// The agent queue `caller` may see: the entries for the workspaces its
    /// scope reaches but its own, where it may queue agents.
    pub(in crate::daemon) async fn visible_queue(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<QueueEntry>> {
        let mut entries = self.queue_entries().await?;
        if let Some(caller) = caller {
            let reached = self.reached_spaces(Some(caller)).await?;
            entries.retain(|entry| {
                entry.workspace().is_some_and(|workspace| {
                    workspace != &caller.space_id
                        && reached
                            .as_ref()
                            .is_none_or(|reached| reached.iter().any(|id| id == workspace))
                })
            });
        }
        Ok(entries)
    }

    /// The workspace `request` starts an agent in, which must be one below
    /// the caller's, refusing a start regardless of the limits.
    pub(in crate::daemon) async fn queue_target<'a>(
        &self,
        caller: &Caller,
        request: &'a mut QueueRequest,
    ) -> Result<&'a mut SpaceSelector> {
        const REFUSAL: &str =
            "workspace processes can start agents only in the workspaces split from theirs";
        ensure!(!request.now, NOT_NOW);
        let target = request.workspace.as_mut().context(REFUSAL)?;
        self.workspace_below(caller, target, REFUSAL).await
    }

    /// Question `number` for the caller to answer, which must be one of a
    /// space its scope reaches other than its own. The answer binds to the
    /// question returned, `shown` when given, and refuses it once a later
    /// question reuses its number.
    pub(in crate::daemon) async fn answerable_question(
        &self,
        caller: &Caller,
        number: ItemNumber,
        shown: Option<Question>,
    ) -> Result<Question> {
        let owner = &caller.space_id;
        let question = match shown {
            Some(shown) => shown,
            None => self.question(number).await?,
        };
        ensure!(
            question.number == number
                && question.space_id != *owner
                && self.reaches(caller, &question.space_id).await?,
            "answering questions is the user's, or a coordinating agent's for the workspaces below it; workspace processes ask with shoal questions ask"
        );
        Ok(question)
    }

    /// The caller's own repository, which `repository` must select, or
    /// for a chat without a repository, whose own is every registered one,
    /// the one it selects; `refusal` says why another is refused.
    pub(in crate::daemon) async fn own_repository(
        &self,
        caller: &Caller,
        repository: &Selector,
        refusal: &str,
    ) -> Result<Selector> {
        let selected = self.repository(repository).await?.id;
        let own = self.caller_repository(caller).await?;
        ensure!(own.is_none_or(|own| own == selected), "{refusal}");
        Ok(selected.into())
    }
}
