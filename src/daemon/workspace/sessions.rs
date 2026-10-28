//! End terminal sessions after ownership ends, including leftovers from older
//! removals. Hosts supply discovery and close operations, not lifecycle policy.
use anyhow::Result;

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
                self.close_session(host, &session.name).await;
            }
        }
        Ok(())
    }

    /// Read ownership fresh for each session: a stale list must not classify a
    /// newly registered workspace as removed. Only the default daemon owns
    /// legacy sessions without a state-directory label.
    pub(crate) async fn cleanup_orphan_sessions(&self) {
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
                self.close_session(host, &session.name).await;
            }
        }
        Ok(())
    }

    /// A demo's place belongs to that launch even when its wrapper was stopped
    /// while disconnected and cannot close the session's shell itself.
    pub(super) async fn close_demo_session(&self, launch: Option<crate::demos::Launch>) {
        let Some(launch) = launch else {
            return;
        };
        if let Some(host) = self
            .session_hosts
            .iter()
            .find(|host| host.name() == launch.host)
        {
            self.close_session(host.as_ref(), &launch.place).await;
        }
    }

    async fn close_session(&self, host: &dyn crate::sessions::Host, name: &str) {
        if let Err(error) = host.close(name).await {
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
