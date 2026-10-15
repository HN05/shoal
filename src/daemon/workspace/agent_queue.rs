//! The agent queue: `shoal add` launches past `max_agents` wait here and start
//! in order as slots free. Slots are counted from what the daemon already
//! tracks, so a limit applies however an agent started.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::Manager;
use crate::{
    daemon::notifications::NotificationKind,
    model::{QueueAdmission, QueueEntry, QueueRequest, QueuedPlan},
    protocol::ConfigTarget,
    state::{ExecutionState, QueueState},
};

/// How long a starting entry keeps its slot without a holder: long enough for
/// a launch to hand over to a Herdr tab or reconnect after a daemon restart.
pub(crate) const START_GRACE: Duration = Duration::from_secs(120);

/// The machine's agent limit and each repository's.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    pub global: Option<u32>,
    pub repositories: HashMap<String, Option<u32>>,
}

impl Limits {
    fn repository(&self, id: &str) -> Option<u32> {
        self.repositories.get(id).copied().flatten()
    }

    fn apply_to(&self, repository: &str) -> bool {
        self.global.is_some() || self.repository(repository).is_some()
    }
}

/// The waiting entries that fit, in queue order. `occupied` holds one
/// repository ID per used slot; `waiting` pairs entry and repository IDs. An
/// entry whose repository is full is passed over; a full machine ends the pass.
pub(crate) fn admit(
    occupied: &[String],
    waiting: &[(String, String)],
    limits: &Limits,
) -> Vec<String> {
    let mut total = occupied.len();
    let mut used: HashMap<&str, usize> = HashMap::new();
    for repository in occupied {
        *used.entry(repository).or_default() += 1;
    }
    let mut admitted = Vec::new();
    for (id, repository) in waiting {
        if limits.global.is_some_and(|limit| total >= limit as usize) {
            break;
        }
        let used = used.entry(repository).or_default();
        if limits
            .repository(repository)
            .is_some_and(|limit| *used >= limit as usize)
        {
            continue;
        }
        *used += 1;
        total += 1;
        admitted.push(id.clone());
    }
    admitted
}

/// An admitted entry the daemon starts itself.
pub(crate) struct Start {
    pub id: String,
    pub name: String,
    pub env: BTreeMap<String, String>,
}

const ENTRY_COLUMNS: &str = "id,repository_id,name,agent,issue_title,state,created_at,workspace_id";

impl Manager {
    /// Admit `request` to a free slot or queue it. A launch no limit applies
    /// to starts untracked; one admitted here starts in the caller.
    pub async fn queue_add(&self, request: QueueRequest) -> Result<QueueAdmission> {
        crate::validate::name("queued workspace", &request.name)?;
        let repository = self.repository(&request.repository).await?;
        let _gate = self.agent_queue_gate.lock().await;
        let limits = self.agent_limits(vec![repository.id.clone()]).await?;
        if !limits.apply_to(&repository.id) {
            return Ok(QueueAdmission {
                start: true,
                entry: None,
                waiting: 0,
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        let state = if request.now {
            QueueState::Starting
        } else {
            QueueState::Queued
        };
        let env = serde_json::to_string(&request.env)?;
        let inserted = id.clone();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                ensure!(
                    !crate::daemon::store::exists(
                        &tx,
                        "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND name=?2",
                        params![repository.id, request.name],
                    )?,
                    "{} is already in the agent queue",
                    request.name
                );
                let now = crate::time::unix_seconds() as i64;
                tx.execute(
                    "INSERT INTO agent_queue(id,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        inserted,
                        repository.id,
                        request.name,
                        request.agent,
                        request.issue_title,
                        request.plan,
                        env,
                        state,
                        now,
                        (state == QueueState::Starting).then_some(now),
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let start = request.now || self.admit_waiting(Some(&id)).await?;
        let entries = self.queue_entries().await?;
        Ok(QueueAdmission {
            start,
            waiting: entries
                .iter()
                .filter(|entry| entry.state == QueueState::Queued)
                .count(),
            entry: entries.into_iter().find(|entry| entry.id == id),
        })
    }

    /// End abandoned starts, then start the waiting entries that fit.
    pub(crate) async fn dispatch_queue(&self) -> Result<()> {
        let _gate = self.agent_queue_gate.lock().await;
        self.expire_starting_entries().await?;
        self.admit_waiting(None).await?;
        Ok(())
    }

    /// Admit waiting entries that fit, start those other than `caller`'s,
    /// and return whether `caller`'s entry was admitted. Requires the queue gate.
    async fn admit_waiting(&self, caller: Option<&str>) -> Result<bool> {
        let waiting = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,repository_id FROM agent_queue WHERE state=?1 ORDER BY seq",
                    )?
                    .query_map([QueueState::Queued], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, String)>>>()?)
            })
            .await?;
        if waiting.is_empty() {
            return Ok(false);
        }
        let repositories = waiting
            .iter()
            .map(|(_, repository)| repository.clone())
            .collect();
        let limits = self.agent_limits(repositories).await?;
        let occupied = self.occupied_slots().await?;
        let admitted = admit(&occupied, &waiting, &limits);
        let starts = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                let now = crate::time::unix_seconds() as i64;
                let mut starts = Vec::new();
                for id in admitted {
                    tx.execute(
                        "UPDATE agent_queue SET state=?2,seen_at=?3 WHERE id=?1",
                        params![id, QueueState::Starting, now],
                    )?;
                    let (name, env): (String, String) = tx.query_row(
                        "SELECT name,env FROM agent_queue WHERE id=?1",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    starts.push(Start {
                        id,
                        name,
                        env: serde_json::from_str(&env).context("read queued environment")?,
                    });
                }
                tx.commit()?;
                Ok(starts)
            })
            .await?;
        let mut caller_admitted = false;
        for start in starts {
            if Some(start.id.as_str()) == caller {
                caller_admitted = true;
                continue;
            }
            match crate::daemon::agent_queue::spawn_start(&self.paths, &start) {
                Ok(()) => {
                    self.notify(
                        Some(&start.name),
                        NotificationKind::QueueStarted,
                        format!("Starting {} from the agent queue", start.name),
                    )
                    .await;
                }
                Err(error) => {
                    self.fail_queue_entry(&start.id, &format!("{error:#}"))
                        .await?
                }
            }
        }
        Ok(caller_admitted)
    }

    /// The global limit and each named repository's effective limit. A
    /// repository without its own value inherits the global one, which the
    /// machine limit already covers.
    async fn agent_limits(&self, repositories: Vec<String>) -> Result<Limits> {
        let config = self.config();
        let mut limits = Limits {
            global: config.max_agents.map(u32::from),
            repositories: HashMap::new(),
        };
        for id in repositories {
            if limits.repositories.contains_key(&id) {
                continue;
            }
            let layers = self
                .config_layers_for(ConfigTarget::Repository(id.clone().into()))
                .await?;
            let limit = config.resolve(&layers)?.max_agents.map(u32::from);
            limits.repositories.insert(id, limit);
        }
        Ok(limits)
    }

    /// One repository ID per used slot. Each agent in a workspace that is not
    /// done uses one while it runs, awaits reattachment or waits for resume,
    /// counted once by its execution; so does each starting entry until its
    /// agent registers.
    async fn occupied_slots(&self) -> Result<Vec<String>> {
        let mut agents = self.agent_executions().await;
        let (open, unknown, starting) = self
            .store
            .run(|db| {
                let open = db
                    .prepare(
                        "SELECT id,repository_id FROM workspaces w WHERE NOT EXISTS
                            (SELECT 1 FROM workspace_completion c WHERE c.workspace_id=w.id)",
                    )?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<HashMap<String, String>>>()?;
                let unknown = db
                    .prepare("SELECT id,workspace_id FROM executions WHERE state=?1")?
                    .query_map([ExecutionState::Unknown], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
                let starting = db
                    .prepare("SELECT repository_id,workspace_id FROM agent_queue WHERE state=?1")?
                    .query_map([QueueState::Starting], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, Option<String>)>>>()?;
                Ok((open, unknown, starting))
            })
            .await?;
        agents.extend(unknown);
        for workspace in open.keys() {
            for execution in crate::execution::recovery::agent_records(&self.paths, workspace)? {
                agents.insert(execution, workspace.clone());
            }
        }
        let mut occupied: Vec<String> = agents
            .values()
            .filter_map(|workspace| open.get(workspace).cloned())
            .collect();
        for (repository, workspace) in starting {
            if workspace.is_none_or(|workspace| open.contains_key(&workspace)) {
                occupied.push(repository);
            }
        }
        Ok(occupied)
    }

    /// End starting entries whose holder has been gone for the grace period.
    /// One that never opened a workspace failed to start, so say so.
    async fn expire_starting_entries(&self) -> Result<()> {
        let held: HashSet<String> = self
            .agent_queue_holds
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let cutoff = crate::time::unix_seconds() as i64 - START_GRACE.as_secs() as i64;
        let expired = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                let stale = tx
                    .prepare(
                        "SELECT id,name,workspace_id FROM agent_queue WHERE state=?1 AND seen_at<?2",
                    )?
                    .query_map(params![QueueState::Starting, cutoff], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String, Option<String>)>>>()?;
                let mut expired = Vec::new();
                for (id, name, workspace) in stale {
                    if held.contains(&id) {
                        continue;
                    }
                    tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
                    expired.push((name, workspace));
                }
                tx.commit()?;
                Ok(expired)
            })
            .await?;
        for (name, _) in expired.iter().filter(|(_, workspace)| workspace.is_none()) {
            self.notify(
                Some(name),
                NotificationKind::QueueFailed,
                format!("{name} did not start from the agent queue: its launch ended without opening the workspace"),
            )
            .await;
        }
        Ok(())
    }

    /// Restart every starting entry's grace period, since no holder could
    /// reconnect while the daemon was away.
    pub(crate) async fn refresh_starting_entries(&self) -> Result<()> {
        let now = crate::time::unix_seconds() as i64;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE state=?1",
                    params![QueueState::Starting, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Entries in queue order, waiting ones numbered.
    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        self.store
            .run(|db| {
                let mut entries = db
                    .prepare(&format!(
                        "SELECT {ENTRY_COLUMNS} FROM agent_queue ORDER BY seq"
                    ))?
                    .query_map([], entry)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut position = 0;
                for entry in &mut entries {
                    if entry.state == QueueState::Queued {
                        position += 1;
                        entry.position = Some(position);
                    }
                }
                Ok(entries)
            })
            .await
    }

    pub async fn queued_plan(&self, id: &str) -> Result<QueuedPlan> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let (entry, plan) = db
                    .query_row(
                        &format!("SELECT {ENTRY_COLUMNS},plan FROM agent_queue WHERE id=?1"),
                        [&id],
                        |row| Ok((entry(row)?, row.get("plan")?)),
                    )
                    .optional()?
                    .context("queue entry no longer exists")?;
                ensure!(
                    entry.state == QueueState::Starting,
                    "queue entry {} is still waiting for a slot",
                    entry.name
                );
                Ok(QueuedPlan { entry, plan })
            })
            .await
    }

    /// Remove a waiting entry by ID or name.
    pub async fn queue_remove(&self, selector: &str) -> Result<QueueEntry> {
        // Dispatch cannot start the entry between finding and deleting it.
        let _gate = self.agent_queue_gate.lock().await;
        let entries = self.queue_entries().await?;
        let mut matches = entries
            .iter()
            .filter(|entry| entry.id == selector || entry.name == selector);
        let entry = matches
            .next()
            .with_context(|| format!("{selector} is not in the agent queue"))?
            .clone();
        ensure!(
            matches.next().is_none(),
            "several queue entries are named {selector}; pass the entry ID"
        );
        ensure!(
            entry.state == QueueState::Queued,
            "{} is already starting; remove its workspace instead",
            entry.name
        );
        let id = entry.id.clone();
        let removed = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Queued],
                )?)
            })
            .await?;
        ensure!(removed == 1, "{} is no longer waiting", entry.name);
        Ok(entry)
    }

    /// Count an open hold on a starting entry.
    pub(crate) async fn hold_queue_entry(&self, id: &str) -> Result<()> {
        ensure!(
            self.queue_entry_starting(id).await?,
            "queue entry is no longer starting"
        );
        *self
            .agent_queue_holds
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default() += 1;
        Ok(())
    }

    /// Drop a hold; the entry's grace period starts once none remain.
    pub(crate) async fn release_queue_hold(&self, id: &str) -> Result<()> {
        {
            let mut holds = self.agent_queue_holds.lock().unwrap();
            if let Some(count) = holds.get_mut(id) {
                *count -= 1;
                if *count == 0 {
                    holds.remove(id);
                }
            }
        }
        let (id, now) = (id.to_owned(), crate::time::unix_seconds() as i64);
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn queue_entry_starting(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Starting],
                )
            })
            .await
    }

    pub async fn queue_attach(&self, id: &str, workspace: &str) -> Result<()> {
        let workspace = self.workspace(workspace).await?;
        let id = id.to_owned();
        let attached = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "UPDATE agent_queue SET workspace_id=?2 WHERE id=?1 AND state=?3",
                    params![id, workspace.id, QueueState::Starting],
                )?)
            })
            .await?;
        ensure!(attached == 1, "queue entry is no longer starting");
        Ok(())
    }

    /// End a starting entry that cannot start its agent and say why.
    pub async fn fail_queue_entry(&self, id: &str, error: &str) -> Result<()> {
        let id = id.to_owned();
        let name = self
            .store
            .run(move |db| {
                let name: Option<String> = db
                    .query_row(
                        "SELECT name FROM agent_queue WHERE id=?1 AND state=?2",
                        params![id, QueueState::Starting],
                        |row| row.get(0),
                    )
                    .optional()?;
                db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Starting],
                )?;
                Ok(name)
            })
            .await?;
        let Some(name) = name else {
            bail!("queue entry is no longer starting");
        };
        self.notify(
            Some(&name),
            NotificationKind::QueueFailed,
            format!("{name} did not start from the agent queue: {error}"),
        )
        .await;
        self.agent_queue_wake.notify_one();
        Ok(())
    }

    /// A registered agent takes over the slot its starting entry kept.
    pub(crate) async fn consume_queue_entries(&self, workspace_id: &str) {
        let workspace_id = workspace_id.to_owned();
        let consumed = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM agent_queue WHERE workspace_id=?1 AND state=?2",
                    params![workspace_id, QueueState::Starting],
                )?)
            })
            .await;
        if let Err(error) = consumed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }
}

fn entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueEntry> {
    Ok(QueueEntry {
        id: row.get("id")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        agent: row.get("agent")?,
        issue_title: row.get("issue_title")?,
        state: row.get("state")?,
        position: None,
        created_at: row.get("created_at")?,
        workspace_id: row.get("workspace_id")?,
    })
}

#[cfg(test)]
mod tests;
