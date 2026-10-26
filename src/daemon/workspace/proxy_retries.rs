//! The request a proxied agent's proxy is retrying, as its wrapper reports
//! it. The daemon only keeps it in memory: the proxy lives and dies with the
//! wrapper, so a restart that forgets it loses nothing that still runs.
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

    /// An execution that ended no longer retries anything.
    pub(super) fn forget_proxy_retry(&self, execution: &str) {
        self.proxy_retries.lock().unwrap().remove(execution);
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
mod tests {
    use super::*;
    use crate::test_support::{manager, repository};

    #[tokio::test]
    async fn a_reported_retry_shows_until_the_proxy_or_its_execution_ends() {
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let started = manager
            .begin_execution(
                &workspace.id,
                None,
                super::super::ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        let execution = started.plan.id.clone();
        let executions = || async {
            manager
                .inspect_workspace(&workspace.name)
                .await
                .unwrap()
                .executions
        };
        let retry = ProxyRetry {
            failure: TurnFailure::Overloaded,
            attempt: 2,
            next_at: 1_000,
        };
        manager
            .report_proxy_retry(&workspace.name, execution.clone(), Some(retry.clone()))
            .await
            .unwrap();
        assert_eq!(
            manager.proxy_retry(&executions().await),
            Some(retry.clone())
        );
        manager
            .report_proxy_retry(&workspace.name, execution.clone(), None)
            .await
            .unwrap();
        assert_eq!(manager.proxy_retry(&executions().await), None);

        manager
            .report_proxy_retry(&workspace.name, execution.clone(), Some(retry))
            .await
            .unwrap();
        manager
            .finish_execution(execution, super::super::ExecutionKind::Command, Some(0))
            .await
            .unwrap();
        assert!(manager.proxy_retries.lock().unwrap().is_empty());
    }
}
