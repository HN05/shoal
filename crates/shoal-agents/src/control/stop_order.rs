//! The running work the stop order reads, from what the daemon records now. A
//! part that cannot be read is left out and logged, so the rest still stops.
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_schedule::stopping::{
    Running, RunningAgent, RunningDemo, RunningJob, RunningRun, StopOrder,
};
use std::collections::{HashMap, HashSet};

use crate::Agents;
use crate::daemon::Daemon;
use crate::log;
use crate::turns::Turn;

impl Agents {
    /// The running work in the order it stops, its agents idle or working
    /// as `turns` say.
    pub async fn stop_order(
        &self,
        daemon: &dyn Daemon,
        turns: &HashMap<ExecutionId, Turn>,
    ) -> StopOrder {
        let running = Running {
            jobs: self.running_jobs(daemon).await,
            runs: self.running_runs(),
            demos: self.running_demos(daemon).await,
            agents: self.stoppable_agents(daemon, turns).await,
            parents: self.space_parents().await,
            unreachable: self.unreachable_agent_spaces().await,
        };
        self.order_stops(daemon, running).await
    }

    /// The claimed CI jobs whose runner's execution runs.
    async fn running_jobs(&self, daemon: &dyn Daemon) -> Vec<RunningJob> {
        daemon.running_jobs().await.unwrap_or_else(|error| {
            log!("CI jobs unavailable to the stop order: {error:#}");
            Vec::new()
        })
    }

    /// The admitted commands and seed builds protection may stop.
    fn running_runs(&self) -> Vec<RunningRun> {
        (self.agent_queue.running_measured().into_iter())
            .map(|run| RunningRun {
                id: run.id,
                space: run.space,
                running_for: run.running_for,
            })
            .collect()
    }

    async fn running_demos(&self, daemon: &dyn Daemon) -> Vec<RunningDemo> {
        daemon.running_demos().await.unwrap_or_else(|error| {
            log!("demos unavailable to the stop order: {error:#}");
            Vec::new()
        })
    }

    /// The connected agents nothing is stopping, a workspace's or a chat's;
    /// without the modes every agent counts as heavy.
    async fn stoppable_agents(
        &self,
        daemon: &dyn Daemon,
        turns: &HashMap<ExecutionId, Turn>,
    ) -> Vec<RunningAgent> {
        let light = self.light(daemon).await.unwrap_or_else(|error| {
            log!("agent weights unavailable to the stop order: {error:#}");
            Default::default()
        });
        let interactions = self.interactions().await.unwrap_or_else(|error| {
            log!("interactions unavailable to the stop order: {error:#}");
            Default::default()
        });
        self.executions
            .with_agents(|agents| {
                (agents.iter())
                    .filter(|(_, agent)| agent.running())
                    .map(|(id, agent)| RunningAgent {
                        execution: id.clone(),
                        space: agent.workspace_id.clone(),
                        light: light.execution(id),
                        running_for: agent.started.elapsed(),
                        interacted_at: interactions.get(&agent.workspace_id).copied(),
                        idle_since: (turns.get(id))
                            .filter(|turn| turn.idle())
                            .map(|turn| turn.since),
                    })
                    .collect()
            })
            .await
    }

    /// The spaces of the agents the daemon records but cannot reach, such
    /// as one whose wrapper has yet to reconnect after a daemon restart,
    /// short of those protection stopped; without the records none.
    async fn unreachable_agent_spaces(&self) -> HashSet<SpaceId> {
        let recorded = (self.store)
            .read(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,space_id FROM executions WHERE agent IS NOT NULL
                         AND id NOT IN (SELECT execution_id FROM agent_stops)",
                    )?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(ExecutionId, SpaceId)>>>()?)
            })
            .await
            .unwrap_or_else(|error| {
                log!("agent records unavailable to the stop order: {error:#}");
                Vec::new()
            });
        let connected: HashSet<ExecutionId> = (self.executions)
            .with_agents(|agents| agents.keys().cloned().collect())
            .await;
        (recorded.into_iter())
            .filter(|(execution, _)| !connected.contains(execution))
            .map(|(_, space)| space)
            .collect()
    }

    /// The parent of each space that has one; without them every agent
    /// counts as a leaf.
    async fn space_parents(&self) -> HashMap<SpaceId, SpaceId> {
        let records = self.records.clone();
        (self.store.read(move |db| records.parent_ids(db)))
            .await
            .unwrap_or_else(|error| {
                log!("space tree unavailable to the stop order: {error:#}");
                HashMap::new()
            })
    }
}
