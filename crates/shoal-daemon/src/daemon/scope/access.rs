//! The named checks and views that confine a scoped caller to what its
//! scope allows.
use anyhow::{Context, Result, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashMap;

use super::Caller;
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        approvals::ApprovalRequest, item_numbers::ItemNumber, manager::Manager,
        permissions::Action, questions::Question,
    },
    model::{ChatQueueRequest, QueueEntry, Repository, RunningAgent, Space, SpaceTarget},
    protocol::operations::WorkspaceStart,
    repository::Selector,
};
use shoal_protocol::space_names::SpaceSelector;

/// Why a scoped caller cannot start the worker of a workspace outside its
/// tree.
const WORKER_REFUSAL: &str =
    "workspace processes can start agents only in the workspaces below theirs";

impl Manager {
    /// Put a space `caller` starts below the space `parent` names, which
    /// its scope must reach, else below its own space.
    pub(in crate::daemon) async fn parent_below(
        &self,
        caller: &Caller,
        parent: &mut Option<SpaceSelector>,
    ) -> Result<()> {
        let id = match parent.as_ref() {
            Some(named) => {
                let space = self.space(named).await?;
                self.ensure_reaches(caller, &space).await?;
                space.id.to_owned()
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

    /// The repository of the caller's own space; none for a chat without
    /// one.
    pub(in crate::daemon) async fn caller_repository(
        &self,
        caller: &Caller,
    ) -> Result<Option<RepositoryId>> {
        let space = self.space_by_id(&caller.space_id).await?;
        Ok(space.repository_id.as_ref().cloned())
    }

    /// The spaces `target` finds among those `caller` sees.
    pub(in crate::daemon) async fn find_visible_spaces(
        &self,
        target: &SpaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Space>> {
        let visible = self.visible_spaces(caller).await?;
        self.find_spaces(target, visible).await
    }

    /// `space`, which must be the caller's own; `refusal` says why
    /// another is refused.
    pub(in crate::daemon) async fn own_workspace<'a>(
        &self,
        caller: &Caller,
        space: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        if !self.names_vouched_own(caller, space) {
            ensure!(self.space(space).await?.id == caller.space_id, "{refusal}");
        }
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
        ensure!(self.space(space).await?.id != caller.space_id, "{refusal}");
        Ok(space)
    }

    /// `space`, which must be one the caller's scope reaches other than its
    /// own, whose stopped agent the caller restores, whoever stopped it.
    pub(in crate::daemon) async fn stopped_below<'a>(
        &self,
        caller: &Caller,
        space: &'a mut SpaceSelector,
    ) -> Result<&'a mut SpaceSelector> {
        const REFUSAL: &str =
            "scoped processes restore the agents of the spaces below their own, not their own";
        let space = self.workspace_below(caller, space, REFUSAL).await?;
        self.ensure_reaches(caller, &self.space(space).await?)
            .await?;
        Ok(space)
    }

    /// Let `caller` remove the space `selector` names, releasing the user's
    /// holds of it and of the spaces below it, as its `user_holds`
    /// permission allows: a removal that `takes` the holds uses up its
    /// approval, and a check before one asks for it but leaves it. Returns
    /// whether the user holds any of them, which only a removal this
    /// authorized releases.
    pub(in crate::daemon) async fn authorize_held_removal(
        &self,
        caller: &Caller,
        selector: &SpaceSelector,
        takes: bool,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        if !self.user_holds_below(&space.id).await? {
            return Ok(false);
        }
        // A name may repeat, so the space's ID binds the approval.
        let removal = format!("remove {} ({}), which the user holds", space.name, space.id);
        let action = Action {
            repository: space.repository_id.as_ref().cloned(),
            ..Action::new(PermissionKey::UserHolds, removal)
        };
        match takes {
            true => self.authorize_action(Some(caller), action).await?,
            false => self.check_action(caller, action).await?,
        }
        Ok(true)
    }

    /// Whether the user holds the space `id` or one below it.
    async fn user_holds_below(&self, id: &SpaceId) -> Result<bool> {
        for id in self.subtree(id).await? {
            if self.user_holds(&id).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Approval request `number`, which must be routed to the caller to
    /// decide, or one its `decide` permission lets it decide as the user
    /// does. Only the stored routing counts; the decision rechecks it with
    /// its grant.
    pub(in crate::daemon) async fn routed_approval(
        &self,
        caller: &Caller,
        number: ItemNumber,
    ) -> Result<ApprovalRequest> {
        let request = self.show_approval(number).await?;
        ensure!(
            request.decider.as_ref() == Some(&caller.space_id)
                || self.decides_as_user(caller, &request).await?,
            "workspace processes decide only the approval requests routed to them, or of the spaces below as their decide permission allows; ask the user to run this command"
        );
        Ok(request)
    }

    /// The spaces `caller` sees, running or stopped: those its scope
    /// reaches, its own first, since scoped commands without a target act
    /// on the first one, and for a chat in a workspace's worktree that
    /// workspace too, last; every space for an unscoped caller.
    pub(in crate::daemon) async fn visible_spaces(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<Space>> {
        let root = self.reach_root(caller).await?;
        let mut spaces = self.spaces_within(root.as_ref()).await?;
        if let Some(root) = &root {
            if let Some(home) = self.chat_home(root).await? {
                spaces.push(self.space_by_id(&home).await?);
            }
            spaces.sort_by_key(|space| space.id != *root);
        }
        Ok(spaces)
    }

    /// The `agents` `caller` may restart: those of the spaces its scope
    /// reaches but its own, or all of them for an unscoped caller.
    pub(in crate::daemon) async fn visible_agents(
        &self,
        caller: Option<&Caller>,
        mut agents: Vec<RunningAgent>,
    ) -> Result<Vec<RunningAgent>> {
        if let Some(caller) = caller {
            let below = self.below_in_reach(caller).await?;
            agents.retain(|agent| below(&agent.space_id));
        }
        Ok(agents)
    }

    /// The `spaces` `caller` may stop: those its scope reaches but its own,
    /// or all of them for an unscoped caller.
    pub(in crate::daemon) async fn spaces_below(
        &self,
        caller: Option<&Caller>,
        mut spaces: Vec<Space>,
    ) -> Result<Vec<Space>> {
        if let Some(caller) = caller {
            let below = self.below_in_reach(caller).await?;
            spaces.retain(|space| below(&space.id));
        }
        Ok(spaces)
    }

    /// Whether `caller`'s scope reaches a space other than its own, as a
    /// test of space IDs.
    async fn below_in_reach<'a>(
        &self,
        caller: &'a Caller,
    ) -> Result<impl Fn(&SpaceId) -> bool + 'a> {
        let reached = self.reached_spaces(Some(caller)).await?;
        Ok(move |space: &SpaceId| {
            space != &caller.space_id
                && (reached.as_ref()).is_none_or(|reached| reached.contains(space))
        })
    }

    /// The agent queue every caller sees: all of it, since positions,
    /// limits and what holds an entry back are machine-wide.
    pub(in crate::daemon) async fn visible_queue(&self) -> Result<Vec<QueueEntry>> {
        self.queue_entries().await
    }

    /// The agent queue entries `caller` may act on: those its scope reaches
    /// but its own space's, where it may queue agents. An entry without a
    /// space of its own is reached through the spaces it is for: a CI job
    /// through the spaces its run is for, a launch of a new space through
    /// its parent.
    pub(in crate::daemon) async fn reachable_queue(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<QueueEntry>> {
        let mut entries = self.queue_entries().await?;
        let Some(caller) = caller else {
            return Ok(entries);
        };
        entries.retain(|entry| entry.space() != Some(&caller.space_id));
        let Some(reached) = self.reached_spaces(Some(caller)).await? else {
            return Ok(entries);
        };
        let parents = self.launch_parents(&entries);
        entries
            .retain(|entry| reached_through(entry, &parents).any(|space| reached.contains(space)));
        Ok(entries)
    }

    /// Refuse the queue entry `id` unless `caller` may act on it, as
    /// [`Self::reachable_queue`] says, such as a launch it starts itself.
    pub(in crate::daemon) async fn ensure_entry_reached(
        &self,
        caller: &Caller,
        id: &QueueEntryId,
    ) -> Result<()> {
        self.reached_entry(caller, id).await.map(drop)
    }

    /// The queue entry `id`, refused unless `caller` may act on it, as
    /// [`Self::ensure_entry_reached`] refuses it.
    pub(in crate::daemon) async fn reached_entry(
        &self,
        caller: &Caller,
        id: &QueueEntryId,
    ) -> Result<QueueEntry> {
        let reachable = self.reachable_queue(Some(caller)).await?;
        (reachable.into_iter())
            .find(|entry| entry.id == *id)
            .with_context(|| format!("queue entry {id} is outside this space's scope"))
    }

    /// Refuse the queue entry `id` that `caller`'s `start` names unless it
    /// is the one launch of the existing workspace `start` reopens, whoever
    /// queued it, which the caller could cancel and queue again.
    pub(in crate::daemon) async fn ensure_own_launch(
        &self,
        caller: &Caller,
        id: &QueueEntryId,
        start: &WorkspaceStart,
    ) -> Result<()> {
        let entry = (self.reachable_queue(Some(caller)).await?.into_iter())
            .find(|entry| entry.id == *id)
            .with_context(|| format!("queue entry {id} is outside this space's scope"))?;
        let own = match start {
            WorkspaceStart::Workspace(workspace) => {
                entry.opens.as_ref() == Some(&self.space(workspace).await?.id)
            }
            _ => false,
        };
        ensure!(own, "queue entry {id} is another launch's");
        Ok(())
    }

    /// Refuse the workspace the launch `entry`, which left the queue,
    /// started its agent in unless `caller`'s scope reaches it.
    pub(in crate::daemon) async fn ensure_started_launch_reached(
        &self,
        caller: &Caller,
        entry: &QueueEntryId,
    ) -> Result<()> {
        let Some(started) = self.started_launch(entry).await? else {
            return Ok(());
        };
        let space = self.space(&SpaceSelector::id(&started)).await?;
        self.ensure_reaches(caller, &space).await
    }

    /// Confine a workspace start of `caller` to its own tree: below its own
    /// space unless `parent` names one below it, from a copy of a workspace
    /// it reaches, or reopening a workspace below its own, which keeps its
    /// place.
    pub(in crate::daemon) async fn confine_workspace_start(
        &self,
        caller: &Caller,
        parent: &mut Option<SpaceSelector>,
        start: &mut crate::protocol::operations::WorkspaceStart,
    ) -> Result<()> {
        use crate::protocol::operations::WorkspaceStart;
        match start {
            WorkspaceStart::Workspace(workspace) => {
                return self.reopened_below(caller, workspace).await;
            }
            WorkspaceStart::Copy { source, .. } => self.copy_within(caller, source).await?,
            _ => {}
        }
        self.parent_below(caller, parent).await
    }

    /// Name by ID the existing workspace `workspace` that `caller`'s launch
    /// reopens for its agent, which must be one its scope reaches other
    /// than its own.
    pub(in crate::daemon) async fn reopened_below(
        &self,
        caller: &Caller,
        workspace: &mut SpaceSelector,
    ) -> Result<()> {
        let workspace = self
            .workspace_below(caller, workspace, WORKER_REFUSAL)
            .await?;
        let space = self.space(workspace).await?;
        self.ensure_reaches(caller, &space).await?;
        *workspace = SpaceSelector::id(&space.id);
        Ok(())
    }

    /// The workspace below `caller`'s own space, which its scope reaches,
    /// whose worker it queues in `mode`, the default mode without one,
    /// refused with `given` permissions the caller does not hold.
    pub(in crate::daemon) async fn worker_target<'a>(
        &self,
        caller: &Caller,
        workspace: &'a mut SpaceSelector,
        mode: Option<&crate::config::modes::ModeName>,
        given: &crate::config::permissions::PermissionsConfig,
    ) -> Result<&'a mut SpaceSelector> {
        let mode = mode
            .cloned()
            .unwrap_or(crate::config::modes::ModeName::DEFAULT);
        let target = self
            .workspace_below(caller, workspace, WORKER_REFUSAL)
            .await?;
        self.ensure_reaches(caller, &self.space(target).await?)
            .await?;
        let giver = Some(&caller.space_id);
        (self.ensure_agent_held(giver, target, &mode, given)).await?;
        Ok(target)
    }

    /// Confine the launch of a new chat's agent that `caller` queues: it
    /// starts in the caller's own repository, with permissions the caller
    /// holds, regardless of the limits as its `limits` permission allows,
    /// as the global config's setup as its `admin` permission allows, and
    /// its chat is recorded at once.
    pub(in crate::daemon) async fn confine_chat_launch(
        &self,
        caller: &Caller,
        request: &mut ChatQueueRequest,
    ) -> Result<()> {
        const REFUSAL: &str = "workspace processes start chats only in their own repository";
        let repository = match &request.repository {
            Some(selector) => {
                let selector = self.own_repository(caller, selector, REFUSAL).await?;
                let id = self.repository(&selector).await?.id;
                request.repository = Some(selector);
                Some(id)
            }
            None => None,
        };
        let giver = Some(&caller.space_id);
        (self.ensure_caller_holds(giver, &request.narrowing, repository.as_ref())).await?;
        if request.mode == crate::config::modes::ModeName::SETUP {
            self.authorize_config_setup(caller).await?;
        }
        if request.now {
            let start = format!("start the agent of new chat {} now", request.name);
            (self.authorize_beyond_limits(caller, start, repository)).await?;
        }
        Ok(())
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

/// The spaces through which a scope reaches `entry`: its own, the spaces a
/// CI job's run is for, or a new space's launch's parent among `parents`.
fn reached_through<'a>(
    entry: &'a QueueEntry,
    parents: &'a HashMap<QueueEntryId, SpaceId>,
) -> impl Iterator<Item = &'a SpaceId> {
    let spaces: Vec<&SpaceId> = match (entry.space(), &entry.job) {
        (Some(space), _) => vec![space],
        (None, Some(job)) => job.space_ids.iter().collect(),
        (None, None) => parents.get(&entry.id).into_iter().collect(),
    };
    spaces.into_iter()
}

#[cfg(test)]
mod tests;
