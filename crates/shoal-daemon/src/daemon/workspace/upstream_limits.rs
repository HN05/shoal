//! The limits proxied agents' proxies found their tools' upstreams under,
//! as their wrappers report them. They live only in memory: the proxy lives
//! and dies with the wrapper, which reports them again.
use std::time::Duration;

use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::time::UnixTime;
use anyhow::Result;

use super::Manager;
pub(crate) use crate::client::executions::UpstreamLimit;

/// How long a limit whose upstream named no end stands after it was last
/// reported: a new agent does not run into it at once, and one finds out
/// soon after it lifted.
pub(crate) const UNNAMED_LIMIT_FOR: Duration = Duration::from_secs(15 * 60);

impl Manager {
    /// Keep the limit the proxy of `execution` in workspace `selector`
    /// found its upstream under, or forget it once there is none.
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
        Ok(())
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
