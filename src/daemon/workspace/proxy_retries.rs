//! The request a proxied agent's proxy is retrying, and when its upstream
//! said it refuses requests until, as its wrapper reports them. The retry
//! lives only in memory: the proxy lives and dies with the wrapper, so a
//! restart that forgets it loses nothing that still runs. The reset is kept
//! with the turn retries it holds back.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::Manager;
use crate::state::TurnFailure;

/// A retry the proxy waits to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRetry {
    pub failure: TurnFailure,
    /// The attempt that failed, from 1.
    pub attempt: u32,
    /// Unix seconds when the proxy tries again.
    pub next_at: i64,
}

impl Manager {
    /// Keep the retry the proxy of `execution` in workspace `selector`
    /// waits to make, or forget it once there is none.
    pub async fn report_proxy_retry(
        &self,
        selector: &str,
        execution: String,
        retry: Option<ProxyRetry>,
    ) -> Result<()> {
        self.workspace(selector).await?;
        let mut retries = self.proxy_retries.lock().unwrap();
        match retry {
            Some(retry) => retries.insert(execution, retry),
            None => retries.remove(&execution),
        };
        Ok(())
    }

    /// Keep when the upstream of `execution`'s proxy refuses requests
    /// until, Unix seconds such as a usage limit's reset, which a retry of
    /// its agent's turn waits past; `None`, once it answers again, forgets
    /// it, so a later failure backs off as its own.
    pub async fn report_upstream_reset(
        &self,
        selector: &str,
        execution: String,
        resets_at: Option<i64>,
    ) -> Result<()> {
        self.workspace(selector).await?;
        self.record_upstream_reset(execution, resets_at).await
    }

    /// An execution that ended no longer retries anything, nor waits for
    /// its upstream's reset.
    pub(super) async fn forget_proxy_retry(&self, execution: &str) {
        self.proxy_retries.lock().unwrap().remove(execution);
        let execution = execution.to_owned();
        let forgot = self
            .store
            .run(move |db| super::turn_retry::forget_upstream_reset(db, &execution))
            .await;
        if let Err(error) = forgot {
            crate::daemon::log!("forget the upstream reset of an ended execution: {error:#}");
        }
    }

    /// The retry the proxy of one of `executions`, the workspace's running
    /// ones, waits to make.
    pub(crate) fn proxy_retry(&self, executions: &[crate::model::Execution]) -> Option<ProxyRetry> {
        let retries = self.proxy_retries.lock().unwrap();
        executions
            .iter()
            .find_map(|execution| retries.get(&execution.id))
            .cloned()
    }
}

#[cfg(test)]
mod tests;
