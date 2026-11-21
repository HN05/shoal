//! The tree of spaces. Every space has at most one parent, a workspace or a
//! chat in its repository, or a chat without a repository, whose agent
//! works on it or coordinates its agent;
//! `space_parents` records it. Every reader takes one walk up or one walk
//! down that link, whatever each space's kind.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::{manager::Manager, notifications::Source};
use crate::daemon::alerts::{Recipients, Sent};
use crate::{
    model::{RefPart, Repository, Space, SpaceKind, SpaceRef, Workspace},
    protocol::CopySource,
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

/// Each space a removal of another takes first, as `space_id, parent_id`:
/// the spaces below it, and the chats in a workspace's worktree, which are
/// not below it but cannot outlive it.
const TAKEN_FIRST: &str = "(SELECT space_id,parent_id FROM space_parents
    UNION ALL SELECT id,workspace_id FROM chats WHERE workspace_id IS NOT NULL)";

impl Manager {
    /// Create a workspace on a new branch named `name` in the repository of
    /// the space `parent`, a workspace or a chat, leaving out what `options`
    /// say, and record `parent` as its parent.
    pub async fn create_child(
        &self,
        parent: &SpaceSelector,
        name: String,
        base: Option<String>,
        options: crate::protocol::CreationOptions,
    ) -> Result<Workspace> {
        let (parent, repo) = self.child_parent(parent, None).await?;
        let (repo, git) = self.repositories.lock(&repo.id).await?;
        crate::git::check_branch_name(&name)?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(
            &repo,
            name,
            branch,
            super::workspace::creation::WorkspaceSource::New {
                base,
                parent: Some(parent.id),
            },
            super::workspace::NewWorkspace {
                options,
                ..Default::default()
            },
            git,
        )
        .await
    }

    /// The space `selector` names, a workspace or a chat, as the parent of
    /// a child workspace, with the repository the child goes in: the
    /// parent's own, or for a chat without one that of the workspace `copy`
    /// copies; a child on a branch of such a chat names none.
    pub(crate) async fn child_parent(
        &self,
        selector: &SpaceSelector,
        copy: Option<&CopySource>,
    ) -> Result<(Space, Repository)> {
        let parent = self.space(selector).await?;
        let repository = match (&parent.repository_id, copy) {
            (Some(own), _) => own.clone(),
            (None, Some(copy)) => self.copied_repository(copy).await?,
            (None, None) => bail!(
                "{} belongs to no repository to create a child workspace in; create it below \
                 a space of the child's repository, or copy a workspace with --copy",
                parent.label()
            ),
        };
        let repository = self.repositories.repository(repository).await?;
        Ok((parent, repository))
    }

    /// Whether `id` is `ancestor` or a space below it.
    pub async fn within_subtree(&self, ancestor: &SpaceId, id: &SpaceId) -> Result<bool> {
        let (ancestor, id) = (ancestor.to_owned(), id.to_owned());
        self.store
            .run(move |db| Ok(subtree(db, &ancestor)?.contains(&id)))
            .await
    }

    /// `id` and every space below it, `id` first.
    pub async fn subtree(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        let id = id.to_owned();
        self.store.run(move |db| subtree(db, &id)).await
    }

    /// Every space a removal of `id` takes before it, nearest first: those
    /// below it and the chats in each workspace's worktree among them.
    pub(crate) async fn spaces_taken_with(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM taken
                            JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                            WHERE depth > 0 ORDER BY depth, spaces.id",
                        taken()
                    ))?
                    .query_map([id], space_ref)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The spaces a removal of `id` takes first, each before those below
    /// it: the chats in its worktree, then the spaces directly below it.
    pub(crate) async fn taken_first(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch
                            FROM {TAKEN_FIRST} AS first
                            JOIN spaces ON spaces.id=first.space_id
                            LEFT JOIN workspaces ON workspaces.id=spaces.id
                            WHERE first.parent_id=?1 ORDER BY spaces.kind, spaces.name"
                    ))?
                    .query_map([id], space_ref)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The spaces above `id`, nearest first.
    pub(crate) async fn ancestors(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM up
                            JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                            WHERE depth > 0 ORDER BY depth",
                        up("true")
                    ))?
                    .query_map([id], space_ref)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The spaces directly below the space `id`, by name.
    pub(crate) async fn children(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store.run(move |db| children(db, &id)).await
    }

    /// Whether a space is below the space `id`, here or on another machine.
    pub(crate) async fn has_spaces_below(&self, id: &SpaceId) -> Result<bool> {
        if !self.children(id).await?.is_empty() {
            return Ok(true);
        }
        Ok(!self.hub.descendants_elsewhere(self, id).await?.is_empty())
    }

    /// Queue `alert` about the space it names for its coordinator: the
    /// nearest space above it with an agent, which receives what arrived
    /// once a queued restore starts it, or, once the tree goes on above
    /// this machine, the nearest on the machines above, which the event
    /// passes up to. `for_user` says the user would receive the event
    /// without a coordinator, which the user then does if the coordinator's
    /// space goes before its agent reads it. Returns where it went, or none
    /// when the alert goes to the user, as [`Self::route`] decides, or its
    /// coordinator's alerts are full; the user then receives the event
    /// instead.
    pub(crate) async fn tell_coordinator(
        &self,
        alert: crate::daemon::alerts::NewAlert,
        for_user: bool,
        source: Source,
    ) -> Option<Sent> {
        let (Some(name), Some(id)) = (alert.about.name.clone(), alert.about.id.clone()) else {
            return None;
        };
        let told = match self.route_up(&id, source).await {
            Ok(Recipients::Coordinator(coordinator)) => {
                let queued = self.queue_alert(&coordinator, alert, for_user).await;
                queued.map(|id| Sent {
                    alerts: vec![id],
                    ..Sent::default()
                })
            }
            Ok(Recipients::Above(above)) => {
                let event = crate::daemon::hub::Event {
                    name: &name,
                    space: &id,
                    kind: alert.kind,
                    for_user,
                    message: alert.message,
                    agent: None,
                    attachments: &alert.attachments,
                };
                // Boxed: on the hub, an event for its own machine's space
                // comes back here when no space there takes it.
                let passed = Box::pin(self.hub.pass_up(self, above, event)).await;
                passed.map(|()| Sent {
                    passed_up: true,
                    ..Sent::default()
                })
            }
            Ok(Recipients::User | Recipients::Spaces(_)) => return None,
            Err(error) => Err(error),
        };
        match told {
            Ok(sent) => Some(sent),
            Err(error) => {
                crate::daemon::log!("message for the coordinator of {name}: {error:#}");
                None
            }
        }
    }

    /// The spaces below `id` whose activity its agent receives: children
    /// without an agent of their own, and theirs in turn.
    #[cfg(test)]
    pub(crate) async fn delegated_spaces(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        let agents = self.tracked_agents().await;
        self.store.run(move |db| delegated(db, &id, &agents)).await
    }

    /// The workspaces whose agent the daemon tracks now.
    pub(crate) async fn tracked_agents(&self) -> TrackedAgents {
        TrackedAgents(self.running_agent_workspaces().await)
    }
}

/// The workspaces whose agent the daemon tracks. An agent counts whether it
/// reports turn states from hooks or not, as custom and Happy agents do not.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrackedAgents(Vec<SpaceId>);

impl TrackedAgents {
    /// The spaces `agents` names as those whose agent the daemon tracks.
    pub(crate) fn of(agents: &[SpaceId]) -> Self {
        Self(agents.to_vec())
    }

    /// The parameter `has_agent` reads.
    fn parameter(&self) -> String {
        serde_json::Value::from(self.0.iter().map(SpaceId::as_str).collect::<Vec<_>>()).to_string()
    }
}

/// The one walk up: a recursive query `up(id, depth)` of the space `?1` at
/// depth 0 and the spaces above it, nearest first, going on above a space
/// only while `climb`, an SQL condition on `up.id`, holds.
fn up(climb: &str) -> String {
    format!(
        "WITH RECURSIVE up(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT space_parents.parent_id, up.depth + 1 FROM up
                JOIN space_parents ON space_parents.space_id=up.id
                WHERE {climb}
        )"
    )
}

/// The one walk down: a recursive query `down(id, depth, path)` of the
/// space `?1` at depth 0 and the spaces below it, each with the IDs from
/// `?1` down to it joined by commas, entering a space below only while
/// `descend`, an SQL condition on its `space_parents.space_id`, holds.
/// The recursive query `taken(id, depth)` of the space `?1` and each space
/// its removal takes before it. [`ensure_parent`] keeps it free of cycles.
fn taken() -> String {
    format!(
        "WITH RECURSIVE taken(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT first.space_id, taken.depth + 1
                FROM taken JOIN {TAKEN_FIRST} AS first ON first.parent_id=taken.id
        )"
    )
}

fn down(descend: &str) -> String {
    format!(
        "WITH RECURSIVE down(id, depth, path) AS (
            SELECT ?1, 0, ?1
            UNION ALL SELECT space_parents.space_id, down.depth + 1,
                    down.path || ',' || space_parents.space_id
                FROM down JOIN space_parents ON space_parents.parent_id=down.id
                WHERE {descend}
        )"
    )
}

/// SQL true when the space in `column` has an agent: one that runs, an
/// agent reporting its turn state, or a queued restore, whose agent
/// receives what arrived once it starts.
fn has_agent(column: &str) -> String {
    format!(
        "({} OR EXISTS (SELECT 1 FROM space_agent_state WHERE space_agent_state.space_id={column})
            OR EXISTS (SELECT 1 FROM agent_queue
                WHERE agent_queue.space_id={column} AND agent_queue.kind='{}'))",
        runs_agent(column),
        crate::state::QueueKind::Restore,
    )
}

/// SQL true when the agent of the space in `column` runs: one among the
/// tracked agents bound as parameter `?2`, or that of a chat that became
/// the workspace until it is handed over to it.
fn runs_agent(column: &str) -> String {
    format!(
        "({column} IN (SELECT value FROM json_each(?2))
            OR EXISTS (SELECT 1 FROM space_conversions WHERE space_conversions.space_id={column}
                AND json_extract(space_conversions.record,'$.to')='workspace'))"
    )
}

/// Whether the agent of the space `id` runs, in any turn state.
pub(crate) fn agent_runs(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<bool> {
    crate::daemon::store::exists(
        db,
        &format!("SELECT 1 WHERE {}", runs_agent("?1")),
        params![id, agents.parameter()],
    )
}

/// Check that `parent` may take the new or top-level space `space` of
/// `repository_id`, none for a chat without one, as its child: a ready
/// workspace or a chat of the same repository, or a chat without a
/// repository, neither below `space` nor in the worktree of a workspace
/// there, since removal takes both first. Removal reserves the parent first and
/// rechecks its children afterwards, so a child either precedes that recheck
/// or is refused here.
pub(crate) fn ensure_parent(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    space: &SpaceId,
    parent: &SpaceId,
) -> Result<()> {
    let name = ready_parent(db, repository_id, parent)?;
    let taken: bool = db.query_row(
        &format!("{} SELECT EXISTS(SELECT 1 FROM taken WHERE id=?2)", taken()),
        [space, parent],
        |row| row.get(0),
    )?;
    ensure!(
        !taken,
        "{name} is below or in the space it would take as a child"
    );
    Ok(())
}

/// The name of the space `parent`, once it takes a child of
/// `repository_id` now: a workspace must be ready.
pub(crate) fn ready_parent(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    parent: &SpaceId,
) -> Result<String> {
    let (name, state) = parent_in_repository(db, repository_id, parent)?;
    if let Some(state) = state {
        ensure!(
            state == WorkspaceState::Ready,
            "parent workspace is {state}; open a child below a ready workspace"
        );
    }
    Ok(name)
}

/// The name of the space `parent` and, for a workspace, its state, once it
/// takes a child of `repository_id`: it is a space of that repository, or a
/// chat without a repository, which takes children in any.
pub(crate) fn parent_in_repository(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    parent: &SpaceId,
) -> Result<(String, Option<WorkspaceState>)> {
    let recorded: Option<(Option<RepositoryId>, String, Option<WorkspaceState>)> = db
        .query_row(
            "SELECT spaces.repository_id,spaces.name,workspaces.state FROM spaces
                LEFT JOIN workspaces USING(id) WHERE spaces.id=?1",
            [parent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((repository, name, state)) = recorded else {
        bail!("parent space {parent} no longer exists");
    };
    ensure!(
        repository.is_none() || repository.as_ref() == repository_id,
        "{name} is in another repository, and only a chat without a repository takes \
         children in other repositories"
    );
    Ok((name, state))
}

/// Record `parent` as the parent of the space `space`, or make it top-level.
pub(crate) fn set_parent(db: &Connection, space: &SpaceId, parent: Option<&SpaceId>) -> Result<()> {
    match parent {
        Some(parent) => db.execute(
            "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)
                ON CONFLICT(space_id) DO UPDATE SET parent_id=excluded.parent_id",
            [space, parent],
        )?,
        None => db.execute("DELETE FROM space_parents WHERE space_id=?1", [space])?,
    };
    Ok(())
}

/// Record `parent` above the new space `space` of `repository_id`, once
/// [`ensure_parent`] finds that it takes it: a parent this machine has not
/// runs on another joined machine, as a restore found, whose own machine
/// decides what it takes.
pub(crate) fn place_below(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    space: &SpaceId,
    parent: &SpaceId,
) -> Result<()> {
    let here = crate::daemon::store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [parent])?;
    if here {
        ensure_parent(db, repository_id, space, parent)?;
    }
    set_parent(db, space, Some(parent))
}

/// The ID of the space directly above the space `id`.
pub(crate) fn parent_id(db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            "SELECT parent_id FROM space_parents WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The space above each space that has one, which may run on another
/// machine, by the ID of the space below it.
pub(crate) fn parent_ids(db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
    Ok(db
        .prepare("SELECT space_id, parent_id FROM space_parents")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// This machine's spaces whose parent it does not have, each with that
/// parent, which runs on another machine.
pub(crate) fn below_elsewhere(db: &Connection) -> Result<Vec<(SpaceId, SpaceId)>> {
    Ok(db
        .prepare(
            "SELECT space_id, parent_id FROM space_parents
                WHERE NOT EXISTS (SELECT 1 FROM spaces WHERE spaces.id=space_parents.parent_id)",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// This machine's spaces whose activity goes to a coordinator on another
/// machine, by the space above them there: each space without an agent
/// whose parent runs elsewhere, then those below it whose activity it
/// would receive, as [`delegated`] walks them.
pub(crate) fn delegated_elsewhere(
    db: &Connection,
    agents: &TrackedAgents,
) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>> {
    let with_agents = with_agents(db, agents)?;
    let mut groups = Vec::new();
    for (space, above) in below_elsewhere(db)? {
        if with_agents.contains(&space) {
            continue;
        }
        let Some(own) = space_ref_of(db, &space)? else {
            continue;
        };
        let spaces = [own].into_iter().chain(delegated(db, &space, agents)?);
        groups.push((above, spaces.collect()));
    }
    Ok(groups)
}

/// The space `id`, if this machine has it.
fn space_ref_of(db: &Connection, id: &SpaceId) -> Result<Option<SpaceRef>> {
    Ok(db
        .query_row(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM spaces
                LEFT JOIN workspaces ON workspaces.id=spaces.id WHERE spaces.id=?1",
            [id],
            space_ref,
        )
        .optional()?)
}

/// The spaces with an agent among `agents`, as [`has_agent`] counts one.
pub(crate) fn with_agents(db: &Connection, agents: &TrackedAgents) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "SELECT id FROM spaces WHERE {}",
            has_agent("spaces.id")
        ))?
        .query_map(params![Option::<String>::None, agents.parameter()], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The first space above the space `id` that this machine does not have,
/// as when its parent runs on another joined machine.
pub(crate) fn first_absent_above(db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            &format!(
                "{} SELECT id FROM up WHERE depth > 0
                    AND NOT EXISTS (SELECT 1 FROM spaces WHERE spaces.id=up.id)
                    ORDER BY depth LIMIT 1",
                up("true")
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// A query of `columns` from the workspace whose ID is parameter 1 and the
/// workspaces below it, through spaces of either kind, or from every
/// workspace when the parameter is NULL, by name.
pub(super) fn subtree_query(columns: &str) -> String {
    format!(
        "{} SELECT {columns} FROM spaces JOIN workspaces USING(id)
            WHERE ?1 IS NULL OR id IN (SELECT id FROM down) ORDER BY name",
        down("true")
    )
}

/// `id` and every space below it, `id` first.
pub(crate) fn subtree(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM down ORDER BY depth, id",
            down("true")
        ))?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces directly below the space `id`, by name.
pub(crate) fn children(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceRef>> {
    Ok(db
        .prepare(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM space_parents
                JOIN spaces ON spaces.id=space_parents.space_id
                LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE space_parents.parent_id=?1 ORDER BY spaces.name",
        )?
        .query_map([id], space_ref)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space directly above the space `id`.
pub(crate) fn parent(db: &Connection, id: &SpaceId) -> Result<Option<SpaceRef>> {
    Ok(db
        .query_row(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM space_parents
                JOIN spaces ON spaces.id=space_parents.parent_id
                LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE space_parents.space_id=?1",
            [id],
            space_ref,
        )
        .optional()?)
}

/// The spaces below `id` whose activity its agent receives.
pub(crate) fn delegated(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<SpaceRef>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM down
                JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                WHERE depth > 0 ORDER BY depth, name",
            down(&format!("NOT {}", has_agent("space_parents.space_id")))
        ))?
        .query_map(params![id, agents.parameter()], space_ref)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space `id` and those below it whose activity its agent receives,
/// each as the path of spaces down to it from the topmost space that
/// receives `id`'s own activity: `id` itself when it has an agent, otherwise
/// the nearest ancestor that has one, or the root.
pub(crate) fn receiving_paths(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<Vec<SpaceId>>> {
    let receivers = receiving_ancestors(db, id, agents)?;
    Ok(delegation_paths(db, id, agents)?
        .into_iter()
        .map(|path| receivers.iter().cloned().chain(path).collect())
        .collect())
}

/// The ancestors that receive `id`'s activity, topmost first: each parent
/// while the space below it has no agent of its own.
fn receiving_ancestors(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM up WHERE depth > 0 ORDER BY depth DESC",
            up(&format!("NOT {}", has_agent("up.id")))
        ))?
        .query_map(params![id, agents.parameter()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The space `id` and those below it whose activity its agent receives,
/// each as the path of spaces from `id` down to it.
fn delegation_paths(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<Vec<SpaceId>>> {
    db.prepare(&format!(
        "{} SELECT path FROM down ORDER BY depth, id",
        down(&format!("NOT {}", has_agent("space_parents.space_id")))
    ))?
    .query_map(params![id, agents.parameter()], |row| {
        row.get::<_, String>(0)
    })?
    .map(|path| {
        Ok(path?
            .split(',')
            .map(|id| SpaceId::from(id.to_owned()))
            .collect())
    })
    .collect()
}

/// The spaces that decide the requests of the space `id`, nearest first:
/// those above it with an agent, then `elsewhere`, those on the machines
/// above. A space without an agent of its own has none, since its
/// coordinator or the user acts there.
pub(crate) fn deciders(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
    elsewhere: Vec<SpaceId>,
) -> Result<Vec<SpaceId>> {
    if !has_running_agent(db, id, agents)? {
        return Ok(Vec::new());
    }
    let mut deciders = spaces_above(db, id, agents)?;
    deciders.extend(elsewhere);
    Ok(deciders)
}

/// The length of the longest chain of spaces whose removals take the
/// space `id` first, each the next one's, as [`TAKEN_FIRST`] links them.
pub(super) fn removal_depth(db: &Connection, id: &SpaceId) -> Result<u32> {
    let sql = format!(
        "WITH RECURSIVE taking(id, depth) AS (
            SELECT ?1, 0
            UNION ALL SELECT first.parent_id, taking.depth + 1
                FROM taking JOIN {TAKEN_FIRST} AS first ON first.space_id=taking.id
        ) SELECT MAX(depth) FROM taking"
    );
    Ok(db.query_row(&sql, [id], |row| row.get(0))?)
}

/// The spaces above the space `id` with an agent, which may decide what it
/// passes up, nearest first.
pub(crate) fn spaces_above(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "{} SELECT id FROM up WHERE depth > 0 AND {} ORDER BY depth",
            up("true"),
            has_agent("up.id")
        ))?
        .query_map(params![id, agents.parameter()], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether the space `id` has an agent among `agents`.
fn has_running_agent(db: &Connection, id: &SpaceId, agents: &TrackedAgents) -> Result<bool> {
    crate::daemon::store::exists(
        db,
        &format!("SELECT 1 WHERE {}", has_agent("?1")),
        params![id, agents.parameter()],
    )
}

/// Where an event a space passes up goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Receiver {
    /// To the agent of this machine's space.
    Here(SpaceId),
    /// To the machine of the space, the first above that this machine has
    /// not, where the tree goes on.
    Elsewhere(SpaceId),
}

/// The coordinator of the space `id` for an event from `source`: the
/// nearest space above it with an agent, or the first one above on
/// another machine. An event about no known space, which names repeat too
/// much to find, has none.
pub(crate) fn coordinator(
    db: &Connection,
    id: Option<SpaceId>,
    source: Source,
    agents: &TrackedAgents,
) -> Result<Option<Receiver>> {
    let Some(id) = id else {
        return Ok(None);
    };
    if source == Source::Agent && !has_running_agent(db, &id, agents)? {
        return Ok(None);
    }
    if let Some(coordinator) = coordinator_of(db, &id, agents)? {
        return Ok(Some(Receiver::Here(coordinator)));
    }
    Ok(first_absent_above(db, &id)?.map(Receiver::Elsewhere))
}

/// What receives an event passed up to the space `id` from below: the
/// nearest space from `id` up with an agent, or the first one above on
/// another machine.
pub(crate) fn receiver_from(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<Receiver>> {
    let here = db
        .query_row(
            &format!(
                "{} SELECT id FROM up WHERE {} ORDER BY depth LIMIT 1",
                up("true"),
                has_agent("up.id")
            ),
            params![id, agents.parameter()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(here) = here {
        return Ok(Some(Receiver::Here(here)));
    }
    Ok(first_absent_above(db, id)?.map(Receiver::Elsewhere))
}

/// The coordinator of the space `id`: the nearest space above it with an
/// agent.
pub(crate) fn coordinator_of(
    db: &Connection,
    id: &SpaceId,
    agents: &TrackedAgents,
) -> Result<Option<SpaceId>> {
    Ok(spaces_above(db, id, agents)?.into_iter().next())
}

/// Fill in the workspace's parent and its children.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.parent = parent(db, &workspace.id)?;
    workspace.children = children(db, &workspace.id)?;
    Ok(())
}

fn space_ref(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceRef> {
    Ok(SpaceRef {
        id: row.get(0)?,
        name: row.get(1)?,
        part: match row.get(2)? {
            SpaceKind::Workspace => RefPart::Workspace {
                branch: row.get(3)?,
            },
            SpaceKind::Chat => RefPart::Chat,
        },
    })
}

mod move_up;
mod parents;

#[cfg(test)]
mod tests;
