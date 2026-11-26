//! The kernel's OOM scores, set from the stop order on Linux every
//! [`INTERVAL`]. Only scores that changed are written. A CI job's processes are
//! those of its runner's scope; every other entry's are those carrying its
//! execution's marker, short of its wrapper, which keeps Shoal's floor.
//! `docs/design/protection.md` says why.
use shoal_ids::execution_ids::ExecutionId;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use shoal_schedule::stopping::{StopOrder, Work};

use crate::Agents;
use crate::daemon::Daemon;
use crate::log;
use crate::model::Execution;
use crate::process::identity;
use crate::process::oom;

/// How often the scores follow the stop order.
const INTERVAL: Duration = Duration::from_secs(10);

/// Lower the daemon's own score to its service manager's, the floor the
/// places and workers it starts keep.
pub fn lower_own_score() {
    match oom::lower_to_manager() {
        Ok(Some(floor)) => log!("OOM score lowered to {floor}, the service manager's"),
        Ok(None) => {}
        Err(error) => log!("OOM score not lowered: {error:#}"),
    }
}

pub async fn run(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) {
    // Logged once until scoring works again.
    let mut failing = false;
    loop {
        match agents.score_for_oom(&*daemon).await {
            Ok(()) => failing = false,
            Err(error) if !std::mem::replace(&mut failing, true) => {
                log!("OOM scores: {error:#}");
            }
            Err(_) => {}
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

/// An entry of the stop order with the processes that take its score.
struct Scored {
    execution: Option<Execution>,
    /// A CI job's, whose scope takes its score.
    job: bool,
}

impl Agents {
    /// Give each entry of the stop order's processes the score of its place.
    async fn score_for_oom(&self, daemon: &dyn Daemon) -> Result<()> {
        let turns = self.protection_turns().await;
        let order = self.stop_order(daemon, &turns).await;
        let entries = self.scored_entries(daemon, &order).await?;
        let ids: HashSet<ExecutionId> = (entries.iter())
            .filter_map(|entry| {
                entry
                    .execution
                    .as_ref()
                    .map(|execution| execution.id.clone())
            })
            .collect();
        let marked = identity::scan_executions(ids).await?;
        let wrappers = self.wrappers().await?;
        let floor = oom::own_score()?;
        let scoring = move || {
            let (wanted, failed) = wanted_scores(&entries, &marked, &wrappers, floor);
            write_scores(wanted);
            failed.map_or(Ok(()), Err)
        };
        tokio::task::spawn_blocking(scoring).await?
    }

    /// Every recorded execution's wrapper, by PID and birth, which keeps
    /// Shoal's floor wherever it runs, as one an agent's command started
    /// does with the agent's marker.
    async fn wrappers(&self) -> Result<Wrappers> {
        let executions = self
            .store
            .read(|db| shoal_store::all_executions(db))
            .await?;
        Ok((executions.into_iter())
            .filter_map(|execution| execution.wrapper)
            .map(|wrapper| (wrapper.pid, wrapper.birth))
            .collect())
    }

    /// The execution of each entry of `order`, in its order: a job's
    /// runner's, a demo's or an agent's.
    async fn scored_entries(&self, daemon: &dyn Daemon, order: &StopOrder) -> Result<Vec<Scored>> {
        let jobs = daemon.job_executions().await?;
        let entries: Vec<(Option<ExecutionId>, bool)> = (order.stops().iter())
            .map(|stop| match &stop.work {
                Work::Job => (jobs.get(&stop.space).cloned(), true),
                Work::Demo(execution) | Work::Agent { execution, .. } => {
                    (Some(execution.clone()), false)
                }
            })
            .collect();
        self.store
            .read(move |db| {
                (entries.into_iter())
                    .map(|(id, job)| {
                        let execution = match id {
                            Some(id) => shoal_store::find_execution(db, &id)?,
                            None => None,
                        };
                        Ok(Scored { execution, job })
                    })
                    .collect()
            })
            .await
    }
}

/// A process to score, as it was found: by its identity, or by the job
/// scope it was listed in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    Marked(identity::Identity),
    Scoped { pid: u32, scope: PathBuf },
}

impl Found {
    fn pid(&self) -> u32 {
        match self {
            Self::Marked(process) => process.pid,
            Self::Scoped { pid, .. } => *pid,
        }
    }

    /// Give the process `score` unless it has it, while it is still the
    /// process found: the same birth, or still in its scope, so a PID
    /// reused since takes no score.
    fn score(&self, score: i16) -> Result<()> {
        match self {
            Self::Marked(process) if identity::alive(process)? => {
                oom::set(process.pid, score)?;
            }
            Self::Marked(_) => {}
            Self::Scoped { pid, scope } => {
                oom::set_in_scope(*pid, scope, score)?;
            }
        }
        Ok(())
    }
}

/// Execution wrappers by PID and birth.
type Wrappers = HashSet<(u32, String)>;

/// The score each process of `entries` takes: those of an entry later in
/// the order, scored lower, win a process two share, and an execution's
/// wrapper among them, which inherited the score of the command that
/// started it, returns to Shoal's `floor`. A job's scope that cannot be read
/// takes no score, and the first such failure comes with the rest.
fn wanted_scores(
    entries: &[Scored],
    marked: &[(ExecutionId, identity::Identity)],
    wrappers: &Wrappers,
    floor: i16,
) -> (Vec<(Found, i16)>, Option<anyhow::Error>) {
    let mut by_execution: HashMap<&ExecutionId, Vec<&identity::Identity>> = HashMap::new();
    for (execution, process) in marked {
        by_execution.entry(execution).or_default().push(process);
    }
    let mut wanted: HashMap<u32, (Found, i16)> = HashMap::new();
    let mut failed = None;
    for (entry, score) in entries.iter().zip(oom::scores(entries.len())) {
        let Some(execution) = &entry.execution else {
            continue;
        };
        let marked = by_execution.get(&execution.id).into_iter().flatten();
        let worker =
            |process: &identity::Identity| wrappers.contains(&(process.pid, process.birth.clone()));
        for process in marked {
            let found = Found::Marked((*process).clone());
            let score = if worker(process) { floor } else { score };
            wanted.insert(process.pid, (found, score));
        }
        let Some(runner) = execution.child.as_ref().filter(|_| entry.job) else {
            continue;
        };
        match job_scope(runner) {
            Ok(scoped) => wanted.extend(
                scoped
                    .into_iter()
                    .map(|found| (found.pid(), (found, score))),
            ),
            Err(error) => failed = failed.or(Some(error)),
        }
    }
    (wanted.into_values().collect(), failed)
}

/// The processes of the scope the job's `runner` runs in, which it has the
/// kernel kill together: its own in the work slice, or its wrapper's where
/// it could start in none; none where it runs in no scope apart from the
/// daemon, or where its PID no longer names the runner, whose scope a
/// process that reused it would name instead.
fn job_scope(runner: &identity::Identity) -> Result<Vec<Found>> {
    if !identity::alive(runner)? {
        return Ok(Vec::new());
    }
    let Some(scope) = oom::scope_of(runner.pid)? else {
        return Ok(Vec::new());
    };
    oom::kill_as_group(&scope)?;
    Ok((oom::scope_processes(&scope)?.into_iter())
        .map(|pid| Found::Scoped {
            pid,
            scope: scope.clone(),
        })
        .collect())
}

/// Write each `wanted` score; a process that ended meanwhile is passed by.
fn write_scores(wanted: Vec<(Found, i16)>) {
    for (found, score) in wanted {
        let _ = found.score(score);
    }
}

#[cfg(test)]
mod tests;
