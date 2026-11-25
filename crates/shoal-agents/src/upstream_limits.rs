//! The limits proxied agents' proxies found their tools' upstreams under,
//! as their wrappers report them. They live only in memory: the proxy lives
//! and dies with the wrapper, which reports them again. The daemon log names
//! each limit reported and lifted, which nothing else keeps.
use std::{collections::HashMap, fmt::Display, time::Duration};

use crate::time::UnixTime;
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{agent::Agent, executions::ServiceLimit};

use crate::Agents;
use crate::daemon::Daemon;
use crate::executions::execution_agent;
use crate::log;
pub use shoal_protocol::executions::UpstreamLimit;

/// How long a limit whose upstream named no end stands after it was last
/// reported: a new agent does not run into it at once, and one finds out
/// soon after it lifted.
pub const UNNAMED_LIMIT_FOR: Duration = Duration::from_secs(15 * 60);

impl Agents {
    /// Keep the limit the proxy of `execution` in space `selector` found
    /// its upstream under, or forget it once there is none, and let the
    /// queue decide again.
    pub async fn report_upstream_limit(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: ExecutionId,
        limit: Option<UpstreamLimit>,
    ) -> Result<()> {
        daemon.space(selector).await?;
        match limit {
            Some(limit) => self.keep_upstream_limit(execution, standing(limit, UnixTime::now())),
            None => self.forget_upstream_limit(&execution, "its proxy is replaced or gone"),
        }
        self.agent_queue.wake();
        Ok(())
    }

    /// Lift the limits every proxy reported for the tool that `execution`
    /// runs, now that the upstream answered its proxy: the tool's queued
    /// agents wait no longer for a limit another agent met, which may have
    /// stopped since.
    pub async fn report_upstream_answered(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: ExecutionId,
    ) -> Result<()> {
        daemon.space(selector).await?;
        let reported = self.upstream_limits.lock().unwrap().clone();
        if reported.is_empty() {
            return Ok(());
        }
        let answered = execution.clone();
        let same = (self.store)
            .read(move |tx| same_tool(tx, &answered, reported))
            .await?;
        self.lift_upstream_limits(same, format!("the upstream answered execution {execution}"));
        self.agent_queue.wake();
        Ok(())
    }

    /// Keep `limit`, standing, for `execution`, logging it when it changed.
    fn keep_upstream_limit(&self, execution: ExecutionId, limit: UpstreamLimit) {
        let previous = self
            .upstream_limits
            .lock()
            .unwrap()
            .insert(execution.clone(), limit);
        if previous != Some(limit) {
            log!("{}", Reported(&execution, limit));
        }
    }

    /// Forget the limit `execution`'s proxy reported, if any, for `why`.
    pub(crate) fn forget_upstream_limit(&self, execution: &ExecutionId, why: impl Display) {
        if let Some(limit) = self.upstream_limits.lock().unwrap().remove(execution) {
            log!("{} lifted: {why}", Reported(execution, limit));
        }
    }

    /// Lift each of `limits`, by the execution that reported it, for `why`,
    /// unless its proxy reported another since, which is newer than what
    /// lifts it.
    fn lift_upstream_limits(&self, limits: Vec<(ExecutionId, UpstreamLimit)>, why: String) {
        let mut standing = self.upstream_limits.lock().unwrap();
        for (execution, limit) in limits {
            if standing.get(&execution) == Some(&limit) {
                standing.remove(&execution);
                log!("{} lifted: {why}", Reported(&execution, limit));
            }
        }
    }

    /// The limit each AI tool stands under at `now`, with the time it
    /// stands until, by the agent whose proxy reported it.
    pub async fn tool_limits(&self, now: UnixTime) -> Result<HashMap<Agent, Standing>> {
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
pub type Standing = (ServiceLimit, UnixTime);

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

/// Of `reported`, the limits of the executions that run the tool
/// `execution` runs, and its own.
fn same_tool(
    db: &rusqlite::Connection,
    execution: &ExecutionId,
    reported: HashMap<ExecutionId, UpstreamLimit>,
) -> Result<Vec<(ExecutionId, UpstreamLimit)>> {
    let tool = execution_agent(db, execution)?;
    let mut same = Vec::new();
    for (reporter, limit) in reported {
        if reporter == *execution || tool.is_some() && execution_agent(db, &reporter)? == tool {
            same.push((reporter, limit));
        }
    }
    Ok(same)
}

/// A limit, by the execution whose proxy reported it, as the daemon log
/// names it.
struct Reported<'a>(&'a ExecutionId, UpstreamLimit);

impl Display for Reported<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(execution, limit) = self;
        let until = limit.until.map(UnixTime::local_seconds).unwrap_or_default();
        write!(
            f,
            "upstream limit {} until {until} reported by execution {execution}",
            limit.limit
        )
    }
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
