//! End terminal sessions after ownership ends, including leftovers from older
//! removals. Hosts supply discovery and close operations, not lifecycle policy.
pub(super) mod pending;

use anyhow::{Context as _, Result};

use super::Manager;
use crate::{daemon::notifications::NotificationKind, model::Workspace};

impl Manager {
    /// Removal is already committed; failures are visible and the next orphan
    /// sweep retries them without restoring workspace ownership.
    pub(super) async fn close_workspace_sessions(&self, workspace: &Workspace) {
        for host in &self.session_hosts {
            if let Err(error) = self
                .close_host_workspace_sessions(host.as_ref(), &workspace.id)
                .await
            {
                self.session_cleanup_failed(Some(&workspace.name), &error)
                    .await;
            }
        }
    }

    async fn close_host_workspace_sessions(
        &self,
        host: &dyn crate::sessions::Host,
        workspace: &str,
    ) -> Result<()> {
        for session in host.sessions(&self.paths.state, true).await? {
            if session.workspace == workspace {
                self.close_session(
                    host,
                    &session.name,
                    &crate::sessions::Owner::Workspace(session.workspace),
                )
                .await;
            }
        }
        Ok(())
    }

    /// Read ownership fresh for each session: a stale list must not classify a
    /// newly registered workspace as removed. Only the default daemon owns
    /// legacy sessions without a state-directory label.
    pub(crate) async fn cleanup_orphan_sessions(&self) {
        self.retry_session_closes(None).await;
        for host in &self.session_hosts {
            if let Err(error) = self.cleanup_host_sessions(host.as_ref()).await {
                self.session_cleanup_failed(None, &error).await;
            }
        }
    }

    async fn cleanup_host_sessions(&self, host: &dyn crate::sessions::Host) -> Result<()> {
        for session in host
            .sessions(&self.paths.state, self.paths.is_default_state())
            .await?
        {
            if self.find_workspace(&session.workspace).await?.is_none() {
                self.close_session(
                    host,
                    &session.name,
                    &crate::sessions::Owner::Workspace(session.workspace),
                )
                .await;
            }
        }
        Ok(())
    }

    /// The close was queued atomically with deleting the verified execution.
    pub(super) async fn close_demo_session(&self, id: Option<String>) {
        if id.is_some() {
            self.retry_session_closes(id).await;
        }
    }

    async fn retry_session_closes(&self, id: Option<String>) {
        let pending = self
            .store
            .run(move |db| pending::list(db, id.as_deref()))
            .await;
        match pending {
            Ok(pending) => {
                for close in pending {
                    if let Err(error) = self.close_pending_session(close).await {
                        self.session_cleanup_failed(None, &error).await;
                    }
                }
            }
            Err(error) => self.session_cleanup_failed(None, &error).await,
        }
    }

    async fn close_pending_session(&self, close: pending::Close) -> Result<()> {
        let host = self
            .session_hosts
            .iter()
            .find(|host| host.name() == close.host)
            .with_context(|| format!("terminal host {} is unavailable", close.host))?;
        host.close(&close.name, &close.owner).await?;
        self.store
            .run(move |db| pending::forget(db, &close.id))
            .await
    }

    async fn close_session(
        &self,
        host: &dyn crate::sessions::Host,
        name: &str,
        owner: &crate::sessions::Owner,
    ) {
        if let Err(error) = host.close(name, owner).await {
            self.session_cleanup_failed(None, &error).await;
        }
    }

    async fn session_cleanup_failed(&self, workspace: Option<&str>, error: &anyhow::Error) {
        self.notify(
            workspace,
            NotificationKind::CleanupFailed,
            &format!("terminal session cleanup failed: {error:#}; retry with shoal cleanup"),
        )
        .await;
    }
}

#[cfg(test)]
mod tests;
