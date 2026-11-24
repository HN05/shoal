//! The named checks and views that confine a scoped caller to what its
//! scope allows.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context, Result, ensure};
use shoal_daemon_base::permitted_actions::Action;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashMap;

use super::Caller;
use crate::{
    approvals::ApprovalRequest,
    config::permissions::PermissionKey,
    item_numbers::ItemNumber,
    model::{QueueEntry, Repository, RunningAgent, Space, SpaceTarget},
    protocol::operations::WorkspaceStart,
    repository::Selector,
};
use shoal_protocol::{hub::directory::HubView, space_names::SpaceSelector};

/// Why a scoped caller cannot start the worker of a workspace outside its
/// tree.
const WORKER_REFUSAL: &str =
    "workspace processes can start agents only in the workspaces below theirs";

impl Coordination {
    /// Put a space `caller` starts below the space `parent` names, which
    /// its scope must reach, else below its own space.
    pub async fn parent_below(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        parent: &mut Option<SpaceSelector>,
    ) -> Result<()> {
        let id = match parent.as_ref() {
            Some(named) => self.reached_space(daemon, caller, named).await?,
            None => caller.space_id.clone(),
        };
        *parent = Some(SpaceSelector::id(&id));
        Ok(())
    }

    /// The ID of the space `named`, which `caller`'s scope must reach: one
    /// here, or one another joined machine runs, as the hub lists the tree.
    pub async fn reached_space(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        named: &SpaceSelector,
    ) -> Result<SpaceId> {
        if daemon.names_vouched_own(caller, named) {
            return Ok(caller.space_id.clone());
        }
        let id = daemon.space_id_anywhere(named).await?;
        match daemon.find_space(&id).await? {
            Some(space) => self.ensure_reaches(daemon, caller, &space).await?,
            None => ensure!(
                daemon.reaches_elsewhere(caller, &id).await?,
                "space {named} is outside this space's scope"
            ),
        }
        Ok(id)
    }

    /// Confine a copy `caller` starts to the workspaces its scope reaches,
    /// live ones only, naming the copied one by ID from then on.
    pub async fn copy_within(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        source: &mut crate::protocol::CopySource,
    ) -> Result<()> {
        let workspace = daemon.workspace(&source.workspace).await.context(
            "workspace processes copy only existing workspaces; ask the user to start from a removed workspace's checkpoint",
        )?;
        ensure!(
            self.reaches(daemon, caller, &workspace.id).await?,
            "workspace processes copy only their own workspace and those below it"
        );
        source.workspace = SpaceSelector::id(&workspace.id);
        Ok(())
    }

    /// The repositories `caller` may see: its own space's, or all of them
    /// for an unscoped caller and a chat without a repository.
    pub async fn visible_repositories(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<Repository>> {
        let mut repositories = daemon.repositories().await?;
        if let Some(caller) = caller
            && let Some(own) = self.caller_repository(daemon, caller).await?
        {
            repositories.retain(|repository| repository.id == own);
        }
        Ok(repositories)
    }

    /// The other joined machines' repositories `caller` sees, with the
    /// hub's machines: every one, or as it sees only its own repository
    /// here, those of its own repository's origin; none on a machine in no
    /// hub.
    pub async fn visible_repositories_elsewhere(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Option<HubView>> {
        let Some(mut view) = daemon.repositories_view().await? else {
            return Ok(None);
        };
        if let Some(caller) = caller
            && let Some(own) = self.caller_repository(daemon, caller).await?
        {
            let own = daemon.repository(own.into()).await?;
            let origin = shoal_core::repository::identity(&own.source).await?;
            (view.directory.repositories)
                .retain(|listed| origin.is_some() && listed.repository.repository.origin == origin);
        }
        Ok(Some(view))
    }

    /// The repository of the caller's own space; none for a chat without
    /// one.
    pub async fn caller_repository(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
    ) -> Result<Option<RepositoryId>> {
        let space = daemon.space_by_id(&caller.space_id).await?;
        Ok(space.repository_id.as_ref().cloned())
    }

    /// The spaces `target` finds among those `caller` sees.
    pub async fn find_visible_spaces(
        &self,
        daemon: &dyn Daemon,
        target: &SpaceTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Space>> {
        let visible = self.visible_spaces(daemon, caller).await?;
        daemon.find_spaces(target, visible).await
    }

    /// `space`, which must be the caller's own; `refusal` says why
    /// another is refused.
    pub async fn own_workspace<'a>(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        space: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        if !daemon.names_vouched_own(caller, space) {
            ensure!(
                daemon.space(space).await?.id == caller.space_id,
                "{refusal}"
            );
        }
        Ok(space)
    }

    /// `space`, which must not be the caller's own; [`super::authorize`]
    /// confines it to the spaces below. `refusal` says why the caller's own
    /// is refused.
    pub async fn workspace_below<'a>(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        space: &'a mut SpaceSelector,
        refusal: &str,
    ) -> Result<&'a mut SpaceSelector> {
        ensure!(
            daemon.space(space).await?.id != caller.space_id,
            "{refusal}"
        );
        Ok(space)
    }

    /// `space`, which must be one the caller's scope reaches other than its
    /// own, whose stopped agent the caller restores, whoever stopped it.
    pub async fn stopped_below<'a>(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        space: &'a mut SpaceSelector,
    ) -> Result<&'a mut SpaceSelector> {
        const REFUSAL: &str =
            "scoped processes restore the agents of the spaces below their own, not their own";
        let space = self.workspace_below(daemon, caller, space, REFUSAL).await?;
        self.ensure_reaches(daemon, caller, &daemon.space(space).await?)
            .await?;
        Ok(space)
    }

    /// Whether removing the space `selector` names at `caller`'s request
    /// releases the user's holds of it and of the spaces below it: the
    /// user's own request always does, and a scoped caller's once its
    /// `user_holds` permission lets it take the holds there are. A hold the
    /// user places after that still refuses the removal.
    pub async fn releases_user_holds(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<bool> {
        match caller {
            Some(caller) => (self.authorize_held_removal(daemon, caller, selector, true)).await,
            None => Ok(true),
        }
    }

    /// Let `caller` remove the space `selector` names, releasing the user's
    /// holds of it and of the spaces below it, as its `user_holds`
    /// permission allows: a removal that `takes` the holds uses up its
    /// approval, and a check before one asks for it but leaves it. Returns
    /// whether the user holds any of them, which only a removal this
    /// authorized releases.
    pub async fn authorize_held_removal(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        selector: &SpaceSelector,
        takes: bool,
    ) -> Result<bool> {
        let space = daemon.space(selector).await?;
        if !self.user_holds_below(daemon, &space.id).await? {
            return Ok(false);
        }
        self.authorize_taking_holds(daemon, caller, &space, takes)
            .await?;
        Ok(true)
    }

    /// Let `caller` take the user's holds of `space`, or with `takes` false
    /// check that it may, as its `user_holds` permission allows.
    pub async fn authorize_taking_holds(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        space: &crate::model::Space,
        takes: bool,
    ) -> Result<()> {
        // A name may repeat, so the space's ID binds the approval.
        let removal = format!("remove {} ({}), which the user holds", space.name, space.id);
        let action = Action {
            repository: space.repository_id.as_ref().cloned(),
            ..Action::new(PermissionKey::UserHolds, removal)
        };
        match takes {
            true => self.authorize_action(daemon, Some(caller), action).await?,
            false => self.check_action(daemon, caller, action).await?,
        }
        Ok(())
    }

    /// Whether the user holds the space `id` or one below it.
    async fn user_holds_below(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        for id in self.subtree(id).await? {
            if daemon.user_holds(&id).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Approval request `number`, which must be routed to the caller to
    /// decide, or one its `decide` permission lets it decide as the user
    /// does. Only the stored routing counts; the decision rechecks it with
    /// its grant.
    pub async fn routed_approval(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        number: ItemNumber,
    ) -> Result<ApprovalRequest> {
        let request = self.show_approval(number).await?;
        ensure!(
            request.decider.as_ref() == Some(&caller.space_id)
                || self.decides_as_user(daemon, caller, &request).await?,
            "workspace processes decide only the approval requests routed to them, or of the spaces below as their decide permission allows; ask the user to run this command"
        );
        Ok(request)
    }

    /// The spaces `caller` sees, running or stopped: those its scope
    /// reaches, its own first, since scoped commands without a target act
    /// on the first one, and for a chat in a workspace's worktree that
    /// workspace too, last; every space for an unscoped caller.
    pub async fn visible_spaces(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<Space>> {
        let root = self.reach_root(caller).await?;
        let mut spaces = daemon.spaces_within(root.as_ref()).await?;
        if let Some(root) = &root {
            if let Some(home) = daemon.chat_home(root).await? {
                spaces.push(daemon.space_by_id(&home).await?);
            }
            spaces.sort_by_key(|space| space.id != *root);
        }
        Ok(spaces)
    }

    /// The `agents` `caller` may restart: those of the spaces its scope
    /// reaches but its own, or all of them for an unscoped caller.
    pub async fn visible_agents(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        mut agents: Vec<RunningAgent>,
    ) -> Result<Vec<RunningAgent>> {
        if let Some(caller) = caller {
            let below = self.below_in_reach(daemon, caller).await?;
            agents.retain(|agent| below(&agent.space_id));
        }
        Ok(agents)
    }

    /// The `spaces` `caller` may stop: those its scope reaches but its own,
    /// or all of them for an unscoped caller.
    pub async fn spaces_below(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        mut spaces: Vec<Space>,
    ) -> Result<Vec<Space>> {
        if let Some(caller) = caller {
            let below = self.below_in_reach(daemon, caller).await?;
            spaces.retain(|space| below(&space.id));
        }
        Ok(spaces)
    }

    /// Whether `caller`'s scope reaches a space other than its own, as a
    /// test of space IDs.
    async fn below_in_reach<'a>(
        &self,
        daemon: &dyn Daemon,
        caller: &'a Caller,
    ) -> Result<impl Fn(&SpaceId) -> bool + 'a> {
        let reached = self.reached_spaces(daemon, Some(caller)).await?;
        Ok(move |space: &SpaceId| {
            space != &caller.space_id
                && (reached.as_ref()).is_none_or(|reached| reached.contains(space))
        })
    }

    /// The agent queue every caller sees: all of it, since positions,
    /// limits and what holds an entry back are machine-wide.
    pub async fn visible_queue(&self, daemon: &dyn Daemon) -> Result<Vec<QueueEntry>> {
        daemon.queue_entries().await
    }

    /// The agent queue entries `caller` may act on: those its scope reaches
    /// but its own space's, where it may queue agents. An entry without a
    /// space of its own is reached through the spaces it is for: a CI job
    /// through the spaces its run is for, a launch of a new space through
    /// its parent.
    pub async fn reachable_queue(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<QueueEntry>> {
        let mut entries = daemon.queue_entries().await?;
        let Some(caller) = caller else {
            return Ok(entries);
        };
        entries.retain(|entry| entry.space() != Some(&caller.space_id));
        let Some(reached) = self.reached_spaces(daemon, Some(caller)).await? else {
            return Ok(entries);
        };
        let parents = daemon.launch_parents(&entries);
        entries
            .retain(|entry| reached_through(entry, &parents).any(|space| reached.contains(space)));
        Ok(entries)
    }

    /// Refuse the queue entry `id` unless `caller` may act on it, as
    /// [`Self::reachable_queue`] says, such as a launch it starts itself.
    pub async fn ensure_entry_reached(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        id: &QueueEntryId,
    ) -> Result<()> {
        self.reached_entry(daemon, caller, id).await.map(drop)
    }

    /// The queue entry `id`, refused unless `caller` may act on it, as
    /// [`Self::ensure_entry_reached`] refuses it.
    pub async fn reached_entry(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        id: &QueueEntryId,
    ) -> Result<QueueEntry> {
        let reachable = self.reachable_queue(daemon, Some(caller)).await?;
        (reachable.into_iter())
            .find(|entry| entry.id == *id)
            .with_context(|| format!("queue entry {id} is outside this space's scope"))
    }

    /// Refuse the queue entry `id` that `caller`'s `start` names unless it
    /// is the one launch of the existing workspace `start` reopens, whoever
    /// queued it, which the caller could cancel and queue again.
    pub async fn ensure_own_launch(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        id: &QueueEntryId,
        start: &WorkspaceStart,
    ) -> Result<()> {
        let entry = (self
            .reachable_queue(daemon, Some(caller))
            .await?
            .into_iter())
        .find(|entry| entry.id == *id)
        .with_context(|| format!("queue entry {id} is outside this space's scope"))?;
        let own = match start {
            WorkspaceStart::Workspace(workspace) => {
                entry.opens.as_ref() == Some(&daemon.space(workspace).await?.id)
            }
            _ => false,
        };
        ensure!(own, "queue entry {id} is another launch's");
        Ok(())
    }

    /// Refuse the workspace the launch `entry`, which left the queue,
    /// started its agent in unless `caller`'s scope reaches it.
    pub async fn ensure_started_launch_reached(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        entry: &QueueEntryId,
    ) -> Result<()> {
        let Some(started) = daemon.started_launch(entry).await? else {
            return Ok(());
        };
        let space = daemon.space(&SpaceSelector::id(&started)).await?;
        self.ensure_reaches(daemon, caller, &space).await
    }

    /// Confine a workspace start of `caller` to its own tree: below its own
    /// space unless `parent` names one below it, from a copy of a workspace
    /// it reaches, or reopening a workspace below its own, which keeps its
    /// place.
    pub async fn confine_workspace_start(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        parent: &mut Option<SpaceSelector>,
        start: &mut crate::protocol::operations::WorkspaceStart,
    ) -> Result<()> {
        use crate::protocol::operations::WorkspaceStart;
        match start {
            WorkspaceStart::Workspace(workspace) => {
                return self.reopened_below(daemon, caller, workspace).await;
            }
            WorkspaceStart::Copy { source, .. } => self.copy_within(daemon, caller, source).await?,
            _ => {}
        }
        self.parent_below(daemon, caller, parent).await
    }

    /// Name by ID the existing workspace `workspace` that `caller`'s launch
    /// reopens for its agent, which must be one its scope reaches other
    /// than its own.
    pub async fn reopened_below(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        workspace: &mut SpaceSelector,
    ) -> Result<()> {
        let workspace = self
            .workspace_below(daemon, caller, workspace, WORKER_REFUSAL)
            .await?;
        let space = daemon.space(workspace).await?;
        self.ensure_reaches(daemon, caller, &space).await?;
        *workspace = SpaceSelector::id(&space.id);
        Ok(())
    }

    /// The workspace below `caller`'s own space, which its scope reaches,
    /// whose worker it queues in `mode`, the default mode without one,
    /// refused with `given` permissions the caller does not hold.
    pub async fn worker_target<'a>(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        workspace: &'a mut SpaceSelector,
        mode: Option<&crate::config::modes::ModeName>,
        given: &crate::config::permissions::PermissionsConfig,
    ) -> Result<&'a mut SpaceSelector> {
        let mode = mode
            .cloned()
            .unwrap_or(crate::config::modes::ModeName::DEFAULT);
        let target = self
            .workspace_below(daemon, caller, workspace, WORKER_REFUSAL)
            .await?;
        self.ensure_reaches(daemon, caller, &daemon.space(target).await?)
            .await?;
        let giver = Some(caller);
        (self.ensure_agent_held(daemon, giver, target, &mode, given)).await?;
        Ok(target)
    }

    /// The caller's own repository, which `repository` must select, or
    /// for a chat without a repository, whose own is every registered one,
    /// the one it selects; `refusal` says why another is refused.
    pub async fn own_repository(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        repository: &Selector,
        refusal: &str,
    ) -> Result<Selector> {
        let selected = daemon.repository(repository.clone()).await?.id;
        let own = self.caller_repository(daemon, caller).await?;
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
