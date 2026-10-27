//! The agent queue: `shoal add` launches past `max_agents` and restores of
//! stopped agents wait here and start in order as slots free. Slots are
//! counted from what the daemon already tracks, so a limit applies however an
//! agent started.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::{
    Manager,
    capacity::{self, Activity, Agents},
};
use crate::{
    config::{AgentLimit, overload::Overload},
    daemon::notifications::NotificationKind,
    model::{QueueAdmission, QueueEntry, QueueRequest, QueuedPlan},
    protocol::ConfigTarget,
    state::{ExecutionState, QueueKind, QueueState},
};

/// How long a starting entry keeps its slot without a holder: long enough for
/// a launch to hand over to a Herdr tab or reconnect after a daemon restart.
pub(crate) const START_GRACE: Duration = Duration::from_secs(120);

/// The machine's agent limits and each repository's.
#[derive(Debug, Default)]
pub(crate) struct Limits {
    /// The machine's fixed slot count.
    pub global: Option<u32>,
    /// How many agents measured load lets start now, under dynamic admission.
    pub starts: Option<u32>,
    pub repositories: HashMap<String, Option<u32>>,
    /// The machine-wide limit free disk space sets while a fixed limit applies.
    pub disk: Option<u32>,
}

impl Limits {
    fn repository(&self, id: &str) -> Option<u32> {
        self.repositories.get(id).copied().flatten()
    }

    fn apply_to(&self, repository: &str) -> bool {
        self.global.is_some() || self.starts.is_some() || self.repository(repository).is_some()
    }
}

/// What a waiting entry starts, as admission sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Work {
    Launch,
    /// A stopped agent's restore. `waiting` says its agent waits for it with
    /// every process stopped.
    Restore {
        waiting: bool,
        wake: Wake,
    },
}

/// What a queued restore waits for besides a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// Sustained healthy readings, for an agent stopped mid-turn, whose slot
    /// `occupied` still counts.
    Recovery,
    /// An update for an agent stopped between turns, which holds no slot
    /// meanwhile; `due` once one waits.
    Update { due: bool },
}

/// A queued entry and the repository it starts an agent in.
pub(crate) struct Waiting {
    pub id: String,
    pub repository: String,
    pub work: Work,
}

/// The waiting entries that fit, in queue order. `occupied` holds one
/// repository ID per used slot. Under fixed limits a launch, like a restore
/// an update woke, needs a free slot and another restore takes back its own;
/// under dynamic admission each needs room for its load. Those other restores
/// start only on sustained healthy readings (`recovered`), one per healthy
/// interval, and nothing queued behind one starts before it. An entry that
/// does not fit is passed over; once nothing more can start the pass ends.
pub(crate) fn admit(
    occupied: &[String],
    waiting: &[Waiting],
    limits: &Limits,
    mut recovered: bool,
) -> Vec<String> {
    let mut total = occupied.len();
    let mut starts = limits.starts;
    let mut used: HashMap<&str, usize> = HashMap::new();
    for repository in occupied {
        *used.entry(repository).or_default() += 1;
    }
    let mut admitted = Vec::new();
    for entry in waiting {
        if starts == Some(0) {
            break;
        }
        let own = match entry.work {
            Work::Launch => 0,
            Work::Restore {
                waiting,
                wake: Wake::Update { due },
            } => match waiting && due {
                true => 0,
                false => continue,
            },
            Work::Restore { .. } if !recovered => break,
            Work::Restore { waiting: false, .. } => continue,
            Work::Restore { waiting: true, .. } => 1,
        };
        let used = used.entry(&entry.repository).or_default();
        let full = |count: usize, limit: u32| count.saturating_sub(own) >= limit as usize;
        if [limits.global, limits.disk]
            .into_iter()
            .flatten()
            .any(|limit| full(total, limit))
            || limits
                .repository(&entry.repository)
                .is_some_and(|limit| full(*used, limit))
        {
            continue;
        }
        *used += 1 - own;
        total += 1 - own;
        starts = starts.map(|starts| starts - 1);
        recovered &= own == 0;
        admitted.push(entry.id.clone());
    }
    admitted
}

/// A waiting row of the queue.
struct QueuedRow {
    id: String,
    repository: String,
    kind: QueueKind,
    execution: Option<String>,
    workspace: Option<String>,
    /// Whether the daemon starts it, rather than a waiting wrapper.
    started_here: bool,
}

/// The waiting entries as admission sees them.
struct WaitingWork {
    entries: Vec<Waiting>,
    /// Each restore a waiting wrapper carries out, by entry ID, with its
    /// execution and wake.
    restores: HashMap<String, (String, Wake)>,
    /// The executions of the stopped agents whose restore the daemon starts,
    /// which hold no slot until it is admitted.
    stopped: HashSet<String>,
}

/// The executions of the `waiting` restores that wait for an update with
/// every process of their agent proven stopped, which hold no slot. One whose
/// processes may still run keeps its slot until they stop.
fn dormant_restores(
    waiting: &[Waiting],
    restores: &HashMap<String, (String, Wake)>,
) -> HashSet<String> {
    waiting
        .iter()
        .filter(|entry| {
            matches!(
                entry.work,
                Work::Restore {
                    waiting: true,
                    wake: Wake::Update { .. },
                }
            )
        })
        .filter_map(|entry| restores.get(&entry.id))
        .map(|(execution, _)| execution.clone())
        .collect()
}

/// The waiting rows of the queue, in order.
fn queued_rows(db: &rusqlite::Connection) -> Result<Vec<QueuedRow>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,kind,execution_id,workspace_id,env IS NOT NULL
             FROM agent_queue WHERE state=?1 ORDER BY seq",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok(QueuedRow {
                id: row.get(0)?,
                repository: row.get(1)?,
                kind: row.get(2)?,
                execution: row.get(3)?,
                workspace: row.get(4)?,
                started_here: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// A launch to record in the queue.
struct Launch {
    id: String,
    repository: String,
    request: QueueRequest,
    state: QueueState,
    /// The existing workspace the launch opens.
    opens: Option<String>,
}

/// Record `launch` unless its repository already queues the same name or is
/// being removed. A launch in an existing workspace needs it ready in the same
/// transaction: removal reserves the workspace before its final checks, so the
/// launch either precedes them, which keeps the workspace, or is refused here.
fn insert_launch(db: &mut rusqlite::Connection, launch: &Launch) -> Result<()> {
    let tx = db.transaction()?;
    ensure!(
        !crate::daemon::store::repository_removing(&tx, &launch.repository)?,
        "the repository is being removed"
    );
    if let Some(workspace) = &launch.opens {
        crate::daemon::store::require_ready(&tx, workspace)
            .context("start agents in a ready workspace")?;
    }
    let request = &launch.request;
    ensure!(
        !crate::daemon::store::exists(
            &tx,
            "SELECT 1 FROM agent_queue WHERE repository_id=?1 AND name=?2 AND kind=?3",
            params![launch.repository, request.name, QueueKind::Launch],
        )?,
        "{} is already in the agent queue",
        request.name
    );
    let now = crate::time::unix_seconds() as i64;
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,opens_workspace_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            launch.id,
            QueueKind::Launch,
            launch.repository,
            request.name,
            request.agent,
            request.issue_title,
            request.plan,
            serde_json::to_string(&request.env)?,
            launch.state,
            now,
            (launch.state == QueueState::Starting).then_some(now),
            launch.opens,
        ],
    )?;
    dependencies::record(&tx, &launch.id, &launch.repository, &request.after)?;
    tx.commit()?;
    Ok(())
}

/// What the store records toward used slots, read in one pass.
struct PersistedSlots {
    /// Each workspace that is not done, by ID, with its repository ID.
    open: HashMap<String, String>,
    /// Executions whose state is unknown: agents that may still run.
    unknown: Vec<UnknownExecution>,
    /// Queue entries that are starting.
    starting: Vec<StartingEntry>,
}

/// An execution in the unknown state.
struct UnknownExecution {
    id: String,
    workspace: String,
}

/// A starting queue entry.
struct StartingEntry {
    repository: String,
    /// The workspace it starts in, once known.
    workspace: Option<String>,
    /// When its holder was last seen.
    seen_at: Option<i64>,
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
        .prepare("SELECT id,workspace_id FROM executions WHERE state=?1")?
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
             WHERE state=?1",
        )?
        .query_map([QueueState::Starting], |row| {
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

/// One used slot.
pub(crate) struct Slot {
    repository: String,
    activity: Activity,
}

/// An admitted entry the daemon starts itself.
pub(crate) struct Start {
    pub id: String,
    pub name: String,
    pub env: BTreeMap<String, String>,
}

const ENTRY_COLUMNS: &str = "id,kind,repository_id,name,agent,issue_title,state,created_at,workspace_id,execution_id,opens_workspace_id,base_branch";

impl Manager {
    /// Admit `request` to a free slot or queue it. A launch that no limit
    /// applies to and that waits for no other work starts untracked; one
    /// admitted here starts in the caller, except a launch in an existing
    /// workspace, which the daemon always starts.
    pub async fn queue_add(&self, request: QueueRequest) -> Result<QueueAdmission> {
        crate::validate::name("queued workspace", &request.name)?;
        ensure!(
            !request.now || request.after.is_empty(),
            "a launch that starts now cannot wait for other work"
        );
        let repository = self.repository(&request.repository).await?;
        let target = match &request.workspace {
            Some(workspace) => Some(self.launch_target(workspace, &repository.id).await?),
            None => None,
        };
        let _gate = self.agent_queue_gate.lock().await;
        // Whether any limit applies does not depend on the slots in use.
        let limits = self.agent_limits(vec![repository.id.clone()], &[]).await?;
        if target.is_none() && request.after.is_empty() && !limits.apply_to(&repository.id) {
            return Ok(QueueAdmission {
                start: true,
                entry: None,
                waiting: 0,
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        // The daemon admits a launch in an existing workspace like a waiting one.
        let state = if request.now && target.is_none() {
            QueueState::Starting
        } else {
            QueueState::Queued
        };
        let start = Start {
            id: id.clone(),
            name: request.name.clone(),
            env: request.env.clone(),
        };
        let launch = Launch {
            id: id.clone(),
            repository: repository.id,
            request,
            state,
            opens: target,
        };
        let detached = launch.opens.is_some();
        let now = launch.request.now && !detached;
        self.store.run(move |db| insert_launch(db, &launch)).await?;
        let admitted = now || self.admit_waiting(Some(&id)).await?;
        // Read the response before a detached worker can take the entry.
        let entries = self.queue_entries().await?;
        if detached && admitted {
            self.start_launches(vec![start], None).await?;
        }
        Ok(QueueAdmission {
            start: admitted && !detached,
            waiting: entries
                .iter()
                .filter(|entry| entry.state == QueueState::Queued)
                .count(),
            entry: entries.into_iter().find(|entry| entry.id == id),
        })
    }

    /// End abandoned starts and restores and those resolved another way,
    /// queue the restores stopped agents request, then start the waiting
    /// entries that fit.
    pub(crate) async fn dispatch_queue(&self) -> Result<()> {
        let _gate = self.agent_queue_gate.lock().await;
        self.expire_starting_entries().await?;
        self.expire_orphaned_restores().await?;
        self.forget_resolved_restores().await?;
        self.queue_requested_restores().await?;
        self.admit_waiting(None).await?;
        Ok(())
    }

    /// Admit waiting entries that fit, start launches other than `caller`'s,
    /// and return whether `caller`'s entry was admitted. An admitted restore
    /// is left to its waiting agent. Requires the queue gate.
    async fn admit_waiting(&self, caller: Option<&str>) -> Result<bool> {
        let WaitingWork {
            entries: mut waiting,
            restores,
            stopped,
        } = self.waiting_work().await?;
        let bases = self.hold_back_dependents(&mut waiting).await?;
        if waiting.is_empty() {
            return Ok(false);
        }
        let repositories = waiting
            .iter()
            .map(|entry| entry.repository.clone())
            .collect();
        let mut dormant = dormant_restores(&waiting, &restores);
        dormant.extend(stopped);
        let slots = self.occupied_slots(&dormant).await?;
        let limits = self.agent_limits(repositories, &slots).await?;
        let occupied: Vec<String> = slots.into_iter().map(|slot| slot.repository).collect();
        let published = self.published_recovery();
        let admitted = admit(&occupied, &waiting, &limits, published.is_some());
        let (woken, recovered): (Vec<_>, Vec<_>) = admitted
            .iter()
            .filter_map(|id| restores.get(id))
            .partition(|(_, wake)| matches!(wake, Wake::Update { .. }));
        if !recovered.is_empty() {
            // A stop, reload or unsafe reading since the check withdrew the
            // readings; the pass it wakes decides again.
            let Some(epoch) = published.and_then(|epoch| self.consume_recovery(epoch)) else {
                return Ok(false);
            };
            self.grant_restores(&recovered, Grant::Recovery(epoch));
        }
        self.grant_restores(&woken, Grant::Update);
        let starts = self.mark_starting(admitted, bases).await?;
        self.start_launches(starts, caller).await
    }

    /// The queued entries in order, each restore with whether its agent
    /// waits and what for. A restore the daemon starts is admitted like a
    /// launch.
    async fn waiting_work(&self) -> Result<WaitingWork> {
        let queued = self.store.run(|db| queued_rows(db)).await?;
        let waiters: HashSet<String> = self
            .restore_waiters
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut restores = HashMap::new();
        let mut stopped = HashSet::new();
        let mut waiting = Vec::new();
        for QueuedRow {
            id,
            repository,
            kind,
            execution,
            workspace,
            started_here,
        } in queued
        {
            let work = match (kind, execution, workspace) {
                (QueueKind::Restore, Some(execution), Some(_)) if started_here => {
                    stopped.insert(execution);
                    Work::Launch
                }
                (QueueKind::Restore, Some(execution), Some(workspace)) => {
                    let waits = waiters.contains(&execution);
                    let wake = self.restore_wake(&workspace, &execution, waits).await?;
                    restores.insert(id.clone(), (execution, wake));
                    Work::Restore {
                        waiting: waits,
                        wake,
                    }
                }
                _ => Work::Launch,
            };
            waiting.push(Waiting {
                id,
                repository,
                work,
            });
        }
        Ok(WaitingWork {
            entries: waiting,
            restores,
            stopped,
        })
    }

    /// Grant the admitted restores of `restores`, by execution.
    fn grant_restores(&self, restores: &[&(String, Wake)], grant: Grant) {
        let mut waiters = self.restore_waiters.lock().unwrap();
        for (execution, _) in restores {
            if let Some(granted) = waiters.get_mut(execution) {
                *granted = Some(grant);
            }
        }
    }

    /// Take the launches whose dependencies are not complete out of `waiting`,
    /// returning the branch each remaining launch with a done dependency
    /// builds on.
    async fn hold_back_dependents(
        &self,
        waiting: &mut Vec<Waiting>,
    ) -> Result<HashMap<String, String>> {
        let readiness = self.dependency_readiness().await?;
        waiting.retain(|entry| {
            !matches!(
                readiness.get(&entry.id),
                Some(dependencies::Readiness::Waiting(_))
            )
        });
        Ok(readiness
            .into_iter()
            .filter_map(|(id, readiness)| match readiness {
                dependencies::Readiness::Ready { base } => Some((id, base?)),
                dependencies::Readiness::Waiting(_) => None,
            })
            .collect())
    }

    /// Mark `admitted` entries starting, each on its base from `bases`,
    /// returning those the daemon starts: launches and the restores that
    /// carry a start environment.
    async fn mark_starting(
        &self,
        admitted: Vec<String>,
        mut bases: HashMap<String, String>,
    ) -> Result<Vec<Start>> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let now = crate::time::unix_seconds() as i64;
                let mut starts = Vec::new();
                for id in admitted {
                    tx.execute(
                        "UPDATE agent_queue SET state=?2,seen_at=?3,base_branch=?4 WHERE id=?1",
                        params![id, QueueState::Starting, now, bases.remove(&id)],
                    )?;
                    let (name, env): (String, Option<String>) = tx.query_row(
                        "SELECT name,env FROM agent_queue WHERE id=?1",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if let Some(env) = env {
                        starts.push(Start {
                            id,
                            name,
                            env: serde_json::from_str(&env).context("read queued environment")?,
                        });
                    }
                }
                tx.commit()?;
                Ok(starts)
            })
            .await
    }

    /// Start each admitted launch other than `caller`'s, which starts in the
    /// caller, and return whether `caller`'s was among them.
    async fn start_launches(&self, starts: Vec<Start>, caller: Option<&str>) -> Result<bool> {
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

    /// The global limits given the `slots` in use, each named repository's
    /// effective limit, and the disk limit when any of them is fixed. A
    /// repository without its own value inherits the global one, which the
    /// machine limit already covers.
    async fn agent_limits(&self, repositories: Vec<String>, slots: &[Slot]) -> Result<Limits> {
        let config = self.config();
        let global = config.max_agents.unwrap_or_default();
        let mut limits = Limits {
            global: fixed_limit(global),
            starts: match global {
                AgentLimit::Dynamic => Some(self.dynamic_starts(&config.overload, slots).await?),
                _ => None,
            },
            repositories: HashMap::new(),
            disk: None,
        };
        for id in repositories {
            if limits.repositories.contains_key(&id) {
                continue;
            }
            let layers = self
                .config_layers_for(ConfigTarget::Repository(id.clone().into()))
                .await?;
            let resolved = config.resolve(&layers)?;
            limits
                .repositories
                .insert(id, resolved.max_agents.and_then(fixed_limit));
        }
        if fixed_limit(global).is_some() || limits.repositories.values().any(Option::is_some) {
            limits.disk = self.disk_limit(&config.overload, slots).await?;
        }
        Ok(limits)
    }

    /// How many agents measured load lets start beside `slots`.
    async fn dynamic_starts(&self, overload: &Overload, slots: &[Slot]) -> Result<u32> {
        let paths = self.admission_paths().await?;
        let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let cpu = self
            .cpu_usage
            .lock()
            .unwrap()
            .admission(tokio::time::Instant::now());
        let limit = capacity::limit(
            capacity::sample(&paths, overload, cpu),
            Agents::count(slots.iter().map(|slot| slot.activity)),
            overload,
        );
        Ok(limit.saturating_sub(slots.len().try_into().unwrap_or(u32::MAX)))
    }

    /// The machine-wide limit free disk space sets for `slots` beside fixed limits.
    async fn disk_limit(&self, overload: &Overload, slots: &[Slot]) -> Result<Option<u32>> {
        let paths = self.admission_paths().await?;
        let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        Ok(capacity::disk_limit(
            capacity::disk_free(&paths, overload),
            slots.len().try_into().unwrap_or(u32::MAX),
            overload,
        ))
    }

    /// The paths whose free space admission reads: daemon state first, then
    /// every workspace.
    async fn admission_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(std::iter::once(self.paths.state.clone())
            .chain(
                self.list_workspaces()
                    .await?
                    .into_iter()
                    .map(|workspace| workspace.path),
            )
            .collect())
    }

    /// The used slots. Each agent in a workspace that is not done uses one
    /// while it runs, awaits reattachment or waits for resume, counted once by
    /// its execution, except the `dormant` ones whose restore waits for an
    /// update; so does each starting launch until its agent registers. An
    /// admitted restore starts in its agent's slot.
    async fn occupied_slots(&self, dormant: &HashSet<String>) -> Result<Vec<Slot>> {
        let mut agents = self.agent_executions().await;
        let PersistedSlots {
            open,
            unknown,
            starting,
        } = self.store.run(|db| persisted_slots(db)).await?;
        let now = crate::time::unix_seconds() as i64;
        let starting_since = |seen_at: Option<i64>| {
            let since = now.saturating_sub(seen_at.unwrap_or(now)).max(0) as u64;
            Activity::Starting(Duration::from_secs(since))
        };
        let stopped = |workspace: String| (workspace, Activity::Stopped);
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
            match &entry.execution {
                Some(execution) => {
                    if let Some((_, activity)) = agents.get_mut(execution) {
                        *activity = starting_since(entry.seen_at);
                    }
                }
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
        Ok(slots)
    }

    /// End starting launches and restores the daemon started whose holder has
    /// been gone for the grace period. A launch that never opened a workspace,
    /// like a restore whose agent never registered, failed to start, so say so.
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
                        "SELECT id,name,kind,workspace_id IS NULL FROM agent_queue
                         WHERE state=?1 AND env IS NOT NULL AND seen_at<?2",
                    )?
                    .query_map(params![QueueState::Starting, cutoff], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String, QueueKind, bool)>>>()?;
                let mut expired = Vec::new();
                for (id, name, kind, unopened) in stale {
                    if held.contains(&id) {
                        continue;
                    }
                    tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
                    let failure = match kind {
                        QueueKind::Launch if unopened => {
                            "its launch ended without opening the workspace"
                        }
                        QueueKind::Launch => continue,
                        QueueKind::Restore => "its restore ended before the agent started",
                    };
                    expired.push((name, failure));
                }
                tx.commit()?;
                Ok(expired)
            })
            .await?;
        for (name, failure) in expired {
            self.notify(
                Some(&name),
                NotificationKind::QueueFailed,
                format!("{name} did not start from the agent queue: {failure}"),
            )
            .await;
        }
        Ok(())
    }

    /// Forget restores whose wrapper has been gone for the grace period, such
    /// as one that never reattached after a daemon restart. Its recovery
    /// record stays for `shoal resume`. A restore the daemon starts has no
    /// wrapper.
    async fn expire_orphaned_restores(&self) -> Result<()> {
        let connected: HashSet<String> = self.connections.lock().await.keys().cloned().collect();
        let cutoff = crate::time::unix_seconds() as i64 - START_GRACE.as_secs() as i64;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let stale = tx
                    .prepare(
                        "SELECT execution_id FROM agent_queue
                         WHERE kind=?1 AND env IS NULL AND seen_at<?2",
                    )?
                    .query_map(params![QueueKind::Restore, cutoff], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                for execution in stale.iter().filter(|id| !connected.contains(*id)) {
                    tx.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Restart every starting launch's and restore's grace period, since no
    /// holder or wrapper could reconnect while the daemon was away.
    pub(crate) async fn refresh_starting_entries(&self) -> Result<()> {
        let now = crate::time::unix_seconds() as i64;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?3 WHERE state=?1 OR kind=?2",
                    params![QueueState::Starting, QueueKind::Restore, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Entries in queue order, waiting ones numbered and with the work they
    /// wait for.
    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        let mut entries = self.numbered_entries().await?;
        let mut readiness = self.dependency_readiness().await?;
        let waking = self.restores_waiting_for_updates().await?;
        for entry in &mut entries {
            if let Some(dependencies::Readiness::Waiting(work)) = readiness.remove(&entry.id) {
                entry.waiting_for = work;
            }
            entry.waits_for_update = waking.contains(&entry.id);
        }
        Ok(entries)
    }

    async fn numbered_entries(&self) -> Result<Vec<QueueEntry>> {
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
                let (entry, plan): (QueueEntry, Option<String>) = db
                    .query_row(
                        &format!("SELECT {ENTRY_COLUMNS},plan FROM agent_queue WHERE id=?1"),
                        [&id],
                        |row| Ok((entry(row)?, row.get("plan")?)),
                    )
                    .optional()?
                    .context("queue entry no longer exists")?;
                let plan = plan.context("queue entry restores an agent; it has no launch plan")?;
                ensure!(
                    entry.state == QueueState::Starting,
                    "queue entry {} is still waiting for a slot",
                    entry.name
                );
                Ok(QueuedPlan { entry, plan })
            })
            .await
    }

    /// Remove a waiting entry by ID or name. A stopped agent whose restore is
    /// removed stays stopped.
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
            entry.kind == QueueKind::Launch || self.restores_stopped_agent(&entry.id).await?,
            "{0} waits to restore its stopped agent; shoal stop {0} cancels the restore",
            entry.name
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
                let tx = db.transaction()?;
                let attached = tx.execute(
                    "UPDATE agent_queue SET workspace_id=?2
                     WHERE id=?1 AND state=?3 AND (opens_workspace_id IS NULL OR opens_workspace_id=?2)",
                    params![id, workspace.id, QueueState::Starting],
                )?;
                if attached == 1 {
                    dependencies::replace_launch(&tx, &id, &workspace.id)?;
                }
                tx.commit()?;
                Ok(attached)
            })
            .await?;
        ensure!(
            attached == 1,
            "queue entry is no longer starting, or starts its agent in another workspace"
        );
        Ok(())
    }

    /// Whether an agent launch in the workspace waits in the queue or starts.
    pub(crate) async fn has_queued_launch(&self, workspace_id: &str) -> Result<bool> {
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE (workspace_id=?1 OR opens_workspace_id=?1) AND kind=?2",
                    params![workspace_id, QueueKind::Launch],
                )
            })
            .await
    }

    /// The ID of the workspace of `repository_id` a launch opens; the
    /// insertion requires it ready.
    async fn launch_target(&self, workspace: &str, repository_id: &str) -> Result<String> {
        let workspace = self.workspace(workspace).await?;
        ensure!(
            workspace.repository_id == repository_id,
            "workspace {} belongs to another repository",
            workspace.name
        );
        Ok(workspace.id)
    }

    /// End a starting entry that cannot start its agent and say why. An entry
    /// that already ended, such as with its removed workspace, has no slot
    /// left to free.
    pub async fn fail_queue_entry(&self, id: &str, error: &str) -> Result<()> {
        let id = id.to_owned();
        let entry = self
            .store
            .run(move |db| {
                let entry: Option<(String, bool)> = db
                    .query_row(
                        "SELECT name, state=?2 FROM agent_queue WHERE id=?1",
                        params![id, QueueState::Starting],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Starting],
                )?;
                Ok(entry)
            })
            .await?;
        let name = match entry {
            None => return Ok(()),
            Some((_, false)) => bail!("queue entry is no longer starting"),
            Some((name, true)) => name,
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

    /// Queue the automatic restore of `execution`, an agent that protection
    /// stopped in `workspace_id`, behind the work already waiting. A wrapper
    /// that reattached while waiting keeps its place and waits again.
    pub(crate) async fn queue_restore(
        &self,
        execution: &str,
        agent: &str,
        workspace_id: &str,
    ) -> Result<()> {
        let workspace = self.workspace(workspace_id).await?;
        let (id, execution, agent) = (
            uuid::Uuid::new_v4().to_string(),
            execution.to_owned(),
            agent.to_owned(),
        );
        self.store
            .run(move |db| {
                let now = crate::time::unix_seconds() as i64;
                db.execute(
                    "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,seen_at,workspace_id,execution_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8,?9)
                     ON CONFLICT(execution_id) DO UPDATE SET state=excluded.state,seen_at=excluded.seen_at",
                    params![
                        id,
                        QueueKind::Restore,
                        workspace.repository_id,
                        workspace.name,
                        agent,
                        QueueState::Queued,
                        now,
                        workspace.id,
                        execution,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Wait until admission starts the queued restore of `execution`, whose
    /// agent waits with every process stopped, and return what admitted it.
    pub(crate) async fn await_restore_admission(&self, execution: &str) -> Result<Grant> {
        let mut changed = self.store.watch_changes();
        let _waiter = RestoreWaiter::register(self, execution);
        self.agent_queue_wake.notify_one();
        loop {
            let id = execution.to_owned();
            let state = self
                .store
                .run(move |db| {
                    Ok(db
                        .query_row(
                            "SELECT state FROM agent_queue WHERE execution_id=?1",
                            [id],
                            |row| row.get(0),
                        )
                        .optional()?)
                })
                .await?;
            match state {
                Some(QueueState::Starting) => {
                    return self
                        .restore_waiters
                        .lock()
                        .unwrap()
                        .get(execution)
                        .copied()
                        .flatten()
                        .context("the restore started without admission");
                }
                Some(QueueState::Queued) => changed.changed().await?,
                None => bail!("the restore is no longer in the agent queue"),
            }
        }
    }

    /// Return `execution`'s revoked restore to its place in the queue.
    pub(crate) async fn requeue_restore(&self, execution: &str) -> Result<()> {
        let execution = execution.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET state=?2 WHERE execution_id=?1",
                    params![execution, QueueState::Queued],
                )?;
                Ok(())
            })
            .await
    }

    /// Start the grace period of `execution`'s restore, whose wrapper lost
    /// its connection and may reattach.
    pub(crate) async fn keep_restore_place(&self, execution: &str) {
        let (execution, now) = (execution.to_owned(), crate::time::unix_seconds() as i64);
        let kept = self
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE execution_id=?1",
                    params![execution, now],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = kept {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }

    /// Take `execution`'s restore out of the queue, once its agent registered
    /// again or the restore ended without it.
    pub(crate) async fn remove_restore(&self, execution: &str) {
        let execution = execution.to_owned();
        let removed = self
            .store
            .run(move |db| {
                db.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
                Ok(())
            })
            .await;
        if let Err(error) = removed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }

    /// The agent a queued launch started takes over the slot its entry kept.
    /// Only that launch's agent names the entry, so another agent registering
    /// in the same workspace leaves it starting.
    pub(crate) async fn consume_queue_entry(&self, id: &str, workspace_id: &str) {
        let (id, workspace_id) = (id.to_owned(), workspace_id.to_owned());
        let consumed = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND workspace_id=?2 AND state=?3",
                    params![id, workspace_id, QueueState::Starting],
                )?)
            })
            .await;
        if let Err(error) = consumed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }
}

/// Marks a restore's agent as waiting for admission while it lives.
struct RestoreWaiter<'a> {
    manager: &'a Manager,
    execution: String,
}

impl<'a> RestoreWaiter<'a> {
    fn register(manager: &'a Manager, execution: &str) -> Self {
        manager
            .restore_waiters
            .lock()
            .unwrap()
            .insert(execution.to_owned(), None);
        Self {
            manager,
            execution: execution.to_owned(),
        }
    }
}

impl Drop for RestoreWaiter<'_> {
    fn drop(&mut self) {
        self.manager
            .restore_waiters
            .lock()
            .unwrap()
            .remove(&self.execution);
    }
}

/// A configured value as a slot count. Dynamic admission measures the whole
/// machine, so it adds no repository limit of its own.
fn fixed_limit(limit: AgentLimit) -> Option<u32> {
    match limit {
        AgentLimit::Fixed(value) => Some(value.get()),
        AgentLimit::Dynamic | AgentLimit::Unlimited => None,
    }
}

fn entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueEntry> {
    Ok(QueueEntry {
        id: row.get("id")?,
        kind: row.get("kind")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        agent: row.get("agent")?,
        issue_title: row.get("issue_title")?,
        state: row.get("state")?,
        position: None,
        created_at: row.get("created_at")?,
        workspace_id: row.get("workspace_id")?,
        execution: row.get("execution_id")?,
        opens: row.get("opens_workspace_id")?,
        waiting_for: Vec::new(),
        waits_for_update: false,
        base: row.get("base_branch")?,
    })
}

mod dependencies;
mod stopped;
mod wake;

pub(crate) use wake::Grant;

#[cfg(test)]
mod tests;
