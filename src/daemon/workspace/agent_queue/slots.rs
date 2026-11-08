//! The slots in use, counted from what the daemon already tracks, so a limit
//! applies however an agent started.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;

use crate::client::space_ids::SpaceId;
use crate::client::time::UnixTime;
use crate::{
    daemon::workspace::{Manager, capacity::Activity},
    state::{ExecutionState, QueueKind, QueueState},
};

/// One used slot.
pub(crate) struct Slot {
    pub repository: String,
    pub activity: Activity,
}

/// What the store records toward used slots, read in one pass.
struct PersistedSlots {
    /// Each workspace that is not done, by ID, with its repository ID.
    open: HashMap<SpaceId, String>,
    /// Executions whose state is unknown, other than CI jobs' runners:
    /// agents that may still run.
    unknown: Vec<UnknownExecution>,
    /// Queue entries that are starting.
    starting: Vec<StartingEntry>,
}

/// An execution in the unknown state.
struct UnknownExecution {
    id: String,
    workspace: SpaceId,
}

/// A starting queue entry.
struct StartingEntry {
    repository: String,
    /// The workspace it starts in, once known.
    workspace: Option<SpaceId>,
    /// When its holder was last seen.
    seen_at: Option<UnixTime>,
    /// The execution a restore resumes; a launch has none.
    execution: Option<String>,
}

/// Read the workspaces, executions and queue entries that slots are counted from.
fn persisted_slots(db: &rusqlite::Connection) -> Result<PersistedSlots> {
    let open = db
        .prepare(
            "SELECT id,repository_id FROM workspaces w WHERE NOT EXISTS
                (SELECT 1 FROM workspace_completion c WHERE c.workspace_id=w.id)",
        )?
        .query_map([], |row| Ok((row.get("id")?, row.get("repository_id")?)))?
        .collect::<rusqlite::Result<_>>()?;
    let unknown = db
        .prepare(
            "SELECT id,workspace_id FROM executions e WHERE state=?1
             AND NOT EXISTS (SELECT 1 FROM ci_jobs j WHERE j.workspace_id=e.workspace_id)",
        )?
        .query_map([ExecutionState::Unknown], |row| {
            Ok(UnknownExecution {
                id: row.get("id")?,
                workspace: row.get("workspace_id")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let starting = db
        .prepare(
            "SELECT repository_id,workspace_id,seen_at,execution_id FROM agent_queue
             WHERE state=?1 AND kind!=?2",
        )?
        .query_map(params![QueueState::Starting, QueueKind::Job], |row| {
            Ok(StartingEntry {
                repository: row.get("repository_id")?,
                workspace: row.get("workspace_id")?,
                seen_at: row.get("seen_at")?,
                execution: row.get("execution_id")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(PersistedSlots {
        open,
        unknown,
        starting,
    })
}

impl Manager {
    /// The used slots. Each agent in a workspace that is not done uses one
    /// while it runs, awaits reattachment or waits for resume, counted once by
    /// its execution, except the `dormant` ones whose restore waits for an
    /// update; so does each starting launch until its agent registers, and
    /// each admitted one-shot run until it ends. An
    /// admitted restore starts in its agent's slot, and one of a last
    /// session, which holds none, as a launch.
    pub(super) async fn occupied_slots(&self, dormant: &HashSet<String>) -> Result<Vec<Slot>> {
        let mut agents = self.agent_executions().await;
        let PersistedSlots {
            open,
            unknown,
            starting,
        } = self.store.run(|db| persisted_slots(db)).await?;
        let now = UnixTime::now();
        let starting_since = |seen_at: Option<UnixTime>| {
            Activity::Starting(now.duration_since(seen_at.unwrap_or(now)))
        };
        let stopped = |workspace: SpaceId| (workspace, Activity::Stopped);
        for UnknownExecution { id, workspace } in unknown {
            agents.entry(id).or_insert_with(|| stopped(workspace));
        }
        for workspace in open.keys() {
            for execution in crate::execution::recovery::agent_records(&self.paths, workspace)? {
                agents
                    .entry(execution)
                    .or_insert_with(|| stopped(workspace.clone()));
            }
        }
        agents.retain(|execution, _| !dormant.contains(execution));
        let mut launches = Vec::new();
        for entry in starting {
            match entry.execution.as_ref().and_then(|id| agents.get_mut(id)) {
                Some((_, activity)) => *activity = starting_since(entry.seen_at),
                None => launches.push(entry),
            }
        }
        let mut slots: Vec<Slot> = agents
            .into_values()
            .filter_map(|(workspace, activity)| {
                Some(Slot {
                    repository: open.get(&workspace)?.clone(),
                    activity,
                })
            })
            .collect();
        for launch in launches {
            if launch
                .workspace
                .as_ref()
                .is_none_or(|workspace| open.contains_key(workspace))
            {
                slots.push(Slot {
                    repository: launch.repository,
                    activity: starting_since(launch.seen_at),
                });
            }
        }
        for (repository, held) in self.agent_queue.admitted_oneshots() {
            slots.push(Slot {
                repository,
                activity: Activity::Running(held),
            });
        }
        Ok(slots)
    }
}
