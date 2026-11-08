//! A restart that leaves its agent to a new wrapper. The old wrapper
//! finishes with the agent's record, which the agent queue restores at once,
//! in the slot the agent kept, and the restart follows the agent to the
//! execution the new wrapper registers, which answers it once the agent runs.
use crate::client::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::{Agent, Manager, Restarts};
use crate::{
    daemon::workspace::agent_queue::Start,
    model::{RunningAgent, Workspace},
    sessions::Origin,
};

/// A restart whose agent starts again in a new wrapper, kept by the
/// execution the agent ran as.
pub(crate) enum Renewal {
    /// The new wrapper has not registered the agent yet.
    Starting(Handover),
    /// The agent runs as this execution now.
    Started(String),
}

/// The restart of an agent whose wrapper finished, until a new one
/// registers it.
pub(crate) struct Handover {
    /// The agent's name and workspace.
    pub(super) name: String,
    pub(super) workspace_id: SpaceId,
    pub(super) restarts: Restarts,
    /// The agent queue entry whose start runs the new wrapper, once recorded.
    entry: Option<String>,
}

/// The restarts that start their agent in a new wrapper, by the execution
/// the agent ran as.
pub(crate) type Renewals = HashMap<String, Renewal>;

impl Agent {
    /// Hand its restart in progress, resumed, over to a new wrapper with
    /// the outcomes of those that ended.
    pub(super) fn hand_over(&mut self) -> Option<Renewal> {
        self.restarts.current.as_mut()?.resume();
        Some(Renewal::Starting(Handover {
            name: self.name.clone(),
            workspace_id: self.workspace_id.clone(),
            restarts: std::mem::take(&mut self.restarts),
            entry: None,
        }))
    }
}

/// Whether a restart left the agent that ran as `id` to a new wrapper that
/// has not registered it yet.
pub(super) fn starting(renewals: &Renewals, id: &str) -> bool {
    matches!(renewals.get(id), Some(Renewal::Starting(_)))
}

impl Handover {
    /// Answer one more request, of `run`, for the restart in progress.
    pub(super) fn answer(
        &mut self,
        run: Option<String>,
    ) -> oneshot::Receiver<Result<Option<Origin>>> {
        match &mut self.restarts.current {
            Some(restart) => restart.answer(run),
            None => {
                let (done, answer) = oneshot::channel();
                let _ = done.send(Err(anyhow!("the restart was cancelled")));
                answer
            }
        }
    }
}

/// The agents whose restart starts them in a new wrapper, each as the
/// execution it ran as.
pub(super) fn handed_over(renewals: &Renewals) -> impl Iterator<Item = RunningAgent> + '_ {
    renewals.iter().filter_map(|(execution, renewal)| {
        let Renewal::Starting(handover) = renewal else {
            return None;
        };
        Some(RunningAgent {
            execution: execution.clone(),
            space_id: handover.workspace_id.clone(),
            agent: Some(handover.name.replace(' ', "-")),
            restorable: true,
            state: None,
            interrupted: false,
            origin: None,
            restarted_by: handover.restarts.restarted_by(),
            restart: handover.restarts.pending(),
            stopped: None,
            restarted_from: earlier(renewals, execution),
        })
    })
}

/// The execution the agent that ran as `id` runs as now, following the
/// restarts that started it in new wrappers.
pub(super) fn current(renewals: &Renewals, id: &str) -> String {
    let mut id = id;
    while let Some(Renewal::Started(next)) = renewals.get(id) {
        id = next;
    }
    id.to_owned()
}

/// The executions the agent of `id` ran as before restarts started it in
/// new wrappers.
pub(super) fn earlier(renewals: &Renewals, id: &str) -> Vec<String> {
    let mut earlier: Vec<String> = renewals
        .keys()
        .filter(|execution| *execution != id && current(renewals, execution) == id)
        .cloned()
        .collect();
    earlier.sort();
    earlier
}

/// Forget what leads to executions that ended without handing their agent
/// over, which no request can reach anymore.
pub(crate) fn forget_ended(renewals: &mut Renewals, agents: &HashMap<String, Agent>) {
    loop {
        let known: HashSet<String> = renewals.keys().cloned().collect();
        let before = renewals.len();
        renewals.retain(|_, renewal| match renewal {
            Renewal::Starting(_) => true,
            Renewal::Started(id) => agents.contains_key(id) || known.contains(id),
        });
        if renewals.len() == before {
            return;
        }
    }
}

impl Manager {
    /// Start the agent of `execution` in `workspace` in a new wrapper for
    /// the restart its wrapper `finished` with, through the agent queue,
    /// where the agent ran. A restart that cannot start it fails, leaving
    /// the record for `shoal resume`; one a stop cancelled starts nothing.
    pub(crate) async fn restart_in_new_wrapper(
        &self,
        workspace: &Workspace,
        execution: &str,
        finished: bool,
    ) {
        if !finished {
            let reason =
                "the stopped agent's wrapper did not finish; its processes are not proven stopped";
            self.fail_renewal(execution, reason.into()).await;
            return;
        }
        let entry = uuid::Uuid::new_v4().to_string();
        // The queue gate keeps dispatch from reading the entry as lost
        // between its publication here and its insertion.
        let gate = self.agent_queue.lock_admission().await;
        if !self.follow_start(execution, &entry).await {
            return;
        }
        let admitted = self
            .admit_stopped_restore(workspace, execution, entry)
            .await;
        drop(gate);
        let started = match admitted {
            Ok(start) => self.spawn_restore(&start).await,
            Err(error) => Err(error),
        };
        if let Err(error) = started {
            self.fail_renewal(execution, format!("{error:#}")).await;
        }
    }

    /// Run the queue start worker for the admitted restore `start`, ending
    /// its entry when it cannot.
    async fn spawn_restore(&self, start: &Start) -> Result<()> {
        let Err(error) = crate::daemon::queued_starts::spawn_start(&self.paths, start) else {
            return Ok(());
        };
        self.fail_queue_entry(&start.id, &format!("{error:#}"))
            .await?;
        Err(error)
    }

    /// Record that the queue entry `entry` starts the new wrapper of the
    /// agent that ran as `execution`; false when its restart was cancelled.
    async fn follow_start(&self, execution: &str, entry: &str) -> bool {
        self.executions
            .with_restarts(|_, renewals| match renewals.get_mut(execution) {
                Some(Renewal::Starting(handover)) => {
                    handover.entry = Some(entry.to_owned());
                    true
                }
                _ => false,
            })
            .await
    }

    /// The agent of `id`, which the queue entry `entry` started, continues
    /// the restart whose start that entry is.
    pub(crate) async fn continue_restart(&self, entry: &str, id: &str) {
        self.executions
            .with_restarts(|agents, renewals| {
                let Some(agent) = agents.get_mut(id) else {
                    return;
                };
                let Some(renewal) = renewals.values_mut().find(|renewal| {
                    matches!(renewal, Renewal::Starting(handover) if handover.entry.as_deref() == Some(entry))
                }) else {
                    return;
                };
                let Renewal::Starting(handover) = std::mem::replace(renewal, Renewal::Started(id.to_owned())) else {
                    return;
                };
                agent.restarts = handover.restarts;
            })
            .await;
    }

    /// End the restart of the agent that ran as `execution` without
    /// starting it again, answering it with `reason`.
    pub(crate) async fn fail_renewal(&self, execution: &str, reason: String) {
        let ended = self
            .executions
            .with_restarts(|_, renewals| match renewals.remove(execution) {
                Some(Renewal::Starting(handover)) => Some(handover),
                other => {
                    if let Some(other) = other {
                        renewals.insert(execution.to_owned(), other);
                    }
                    None
                }
            })
            .await;
        if let Some(handover) = ended {
            answer_all(handover, &reason);
        }
    }

    /// The workspaces whose agent a restart starts in a new wrapper that
    /// has not registered it yet.
    pub(crate) async fn renewing_workspaces(&self) -> HashSet<SpaceId> {
        self.executions
            .with_restarts(|_, renewals| {
                renewals
                    .values()
                    .filter_map(|renewal| match renewal {
                        Renewal::Starting(handover) => Some(handover.workspace_id.clone()),
                        Renewal::Started(_) => None,
                    })
                    .collect()
            })
            .await
    }

    /// Cancel the restarts that would start an agent of `workspace_id` in a
    /// new wrapper, leaving its record for `shoal resume`. Requires the
    /// queue gate, which a restart holds while it records its restore.
    pub(crate) async fn cancel_renewals(&self, workspace_id: &SpaceId) {
        let cancelled: Vec<Handover> = self
            .executions
            .with_restarts(|_, renewals| {
                let ids: Vec<String> = renewals
                    .iter()
                    .filter(|(_, renewal)| {
                        matches!(renewal, Renewal::Starting(handover) if handover.workspace_id == *workspace_id)
                    })
                    .map(|(execution, _)| execution.clone())
                    .collect();
                ids.iter()
                    .filter_map(|execution| match renewals.remove(execution) {
                        Some(Renewal::Starting(handover)) => Some(handover),
                        _ => None,
                    })
                    .collect()
            })
            .await;
        for handover in cancelled {
            answer_all(handover, "the restart was cancelled");
        }
    }

    /// End the restart whose new wrapper the queue entry `entry` started,
    /// which ended with `reason` before the agent registered.
    pub(crate) async fn fail_renewal_started_by(&self, entry: &str, reason: &str) {
        let execution = self
            .executions
            .with_restarts(|_, renewals| {
                renewals
                    .iter()
                    .find(|(_, renewal)| {
                        matches!(renewal, Renewal::Starting(handover) if handover.entry.as_deref() == Some(entry))
                    })
                    .map(|(execution, _)| execution.clone())
            })
            .await;
        if let Some(execution) = execution {
            self.fail_renewal(&execution, reason.to_owned()).await;
        }
    }

    /// End the restarts whose new wrapper's start left the agent queue
    /// before the agent registered, as a stop that cancels it or an expired
    /// start does. The restarts are read first, so a start recorded since
    /// is in the queue read after them.
    pub(crate) async fn fail_lost_renewals(&self) -> Result<()> {
        let starting: Vec<(String, String)> = self
            .executions
            .with_restarts(|_, renewals| {
                renewals
                    .iter()
                    .filter_map(|(execution, renewal)| match renewal {
                        Renewal::Starting(Handover {
                            entry: Some(entry), ..
                        }) => Some((execution.clone(), entry.clone())),
                        _ => None,
                    })
                    .collect()
            })
            .await;
        if starting.is_empty() {
            return Ok(());
        }
        let queued = self.store.run(|db| queued_ids(db)).await?;
        for (execution, entry) in starting {
            if !queued.contains(&entry) {
                let reason = "its start from the agent queue ended before the agent ran";
                self.fail_renewal(&execution, reason.into()).await;
            }
        }
        Ok(())
    }
}

/// The IDs of every entry in the agent queue.
fn queued_ids(db: &rusqlite::Connection) -> Result<HashSet<String>> {
    Ok(db
        .prepare("SELECT id FROM agent_queue")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Answer every request for the restart `handover` carries, which ended
/// without starting the agent again, with `reason`.
fn answer_all(handover: Handover, reason: &str) {
    let answers = handover.restarts.current.map(|restart| restart.answers);
    for done in answers.into_iter().flatten() {
        // The request may have gone with its client.
        let _ = done.send(Err(anyhow!(reason.to_owned())));
    }
}
