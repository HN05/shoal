//! The limits proxied agents' proxies found their tools' upstreams under,
//! as their wrappers report them. They live only in memory: the proxy lives
//! and dies with the wrapper, which reports them again.
use std::{collections::HashMap, time::Duration};

use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::client::{agent::Agent, executions::ServiceLimit};
use crate::time::UnixTime;
use anyhow::Result;
use rusqlite::OptionalExtension;

use super::Manager;
pub(crate) use crate::client::executions::UpstreamLimit;

/// How long a limit whose upstream named no end stands after it was last
/// reported: a new agent does not run into it at once, and one finds out
/// soon after it lifted.
pub(crate) const UNNAMED_LIMIT_FOR: Duration = Duration::from_secs(15 * 60);

impl Manager {
    /// Keep the limit the proxy of `execution` in workspace `selector`
    /// found its upstream under, or forget it once there is none, and let
    /// the queue decide again.
    pub async fn report_upstream_limit(
        &self,
        selector: &SpaceSelector,
        execution: ExecutionId,
        limit: Option<UpstreamLimit>,
    ) -> Result<()> {
        self.workspace(selector).await?;
        let mut limits = self.upstream_limits.lock().unwrap();
        match limit {
            Some(limit) => limits.insert(execution, standing(limit, UnixTime::now())),
            None => limits.remove(&execution),
        };
        drop(limits);
        self.agent_queue.wake();
        Ok(())
    }

    /// The limit each AI tool stands under at `now`, with the time it
    /// stands until, by the agent whose proxy reported it.
    pub(crate) async fn tool_limits(&self, now: UnixTime) -> Result<HashMap<Agent, Standing>> {
        let standing = self.standing_limits(now);
        if standing.is_empty() {
            return Ok(HashMap::new());
        }
        let tools = self
            .store
            .read(move |tx| {
                let mut tools = Vec::new();
                for (execution, limit) in standing {
                    if let Some(tool) = execution_agent(tx, &execution)? {
                        tools.push((tool, limit));
                    }
                }
                Ok(tools)
            })
            .await?;
        Ok(longest_by_tool(tools))
    }

    /// The limits that still stand at `now`, by the execution whose proxy
    /// reported each.
    fn standing_limits(&self, now: UnixTime) -> Vec<(ExecutionId, Standing)> {
        (self.upstream_limits.lock().unwrap().iter())
            .filter_map(|(execution, limit)| {
                let until = limit.until.filter(|until| *until > now)?;
                Some((execution.clone(), (limit.limit, until)))
            })
            .collect()
    }
}

/// A limit with the time it stands until.
pub(crate) type Standing = (ServiceLimit, UnixTime);

/// Each tool's limit of `limits`: of several, the one that stands longest.
fn longest_by_tool(limits: Vec<(Agent, Standing)>) -> HashMap<Agent, Standing> {
    let mut longest = HashMap::new();
    for (tool, limit) in limits {
        let kept = longest.entry(tool).or_insert(limit);
        if limit.1 > kept.1 {
            *kept = limit;
        }
    }
    longest
}

/// The agent `execution` runs, if it runs one.
fn execution_agent(db: &rusqlite::Connection, execution: &ExecutionId) -> Result<Option<Agent>> {
    Ok(db
        .query_row(
            "SELECT agent FROM executions WHERE id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// `limit`, reported at `now`, with the time it stands until.
fn standing(limit: UpstreamLimit, now: UnixTime) -> UpstreamLimit {
    UpstreamLimit {
        until: Some(limit.until.unwrap_or(now + UNNAMED_LIMIT_FOR)),
        ..limit
    }
}

#[cfg(test)]
mod tests;
