//! End terminal sessions Shoal created once their owner ends, including
//! leftovers from failed closes. Hosts supply discovery and close operations,
//! not lifecycle policy. Sessions a host labels as Shoal's are found there;
//! places a host does not label are closed from the daemon's records of the
//! places Shoal created.
pub(super) mod pending;

use anyhow::{Context as _, Result};

use super::Manager;
use crate::{
    daemon::notifications::NotificationKind,
    model::Workspace,
    sessions::{HostName, Origin, Owner, PlaceClose},
};

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
        self.close_owned_places(Some(workspace.id.clone())).await;
    }

    async fn close_host_workspace_sessions(
        &self,
        host: &dyn crate::sessions::Host,
        workspace: &str,
    ) -> Result<()> {
        for session in host.sessions(&self.paths.state).await? {
            if session.workspace == workspace {
                self.close_session(host, &session.name, Owner::Workspace(session.workspace))
                    .await;
            }
        }
        Ok(())
    }

    /// Read ownership fresh for each session: a stale list must not classify a
    /// newly registered workspace as removed.
    pub(crate) async fn cleanup_orphan_sessions(&self) {
        self.retry_session_closes(None).await;
        for host in &self.session_hosts {
            if let Err(error) = self.cleanup_host_sessions(host.as_ref()).await {
                self.session_cleanup_failed(None, &error).await;
            }
        }
        self.close_owned_places(None).await;
    }

    async fn cleanup_host_sessions(&self, host: &dyn crate::sessions::Host) -> Result<()> {
        for session in host.sessions(&self.paths.state).await? {
            if self.find_workspace(&session.workspace).await?.is_none() {
                self.close_session(host, &session.name, Owner::Workspace(session.workspace))
                    .await;
            }
        }
        Ok(())
    }

    /// Close the places Shoal created for `workspace`, which removal ended,
    /// or for every workspace whose record is gone.
    async fn close_owned_places(&self, workspace: Option<String>) {
        let sweep = workspace.is_none();
        let places = match self.owned_places(workspace).await {
            Ok(places) => places,
            Err(error) => return self.session_cleanup_failed(None, &error).await,
        };
        for (owner, place) in places {
            let removed = !sweep || matches!(self.find_workspace(&owner).await, Ok(None));
            if removed
                && let Err(error) = self
                    .close_recorded_place(&owner, place.host, place.id())
                    .await
            {
                self.session_cleanup_failed(None, &error).await;
            }
        }
    }

    /// Close `place` of `host`, recorded as one Shoal created, and forget
    /// it while its record still names `workspace` its owner: an agent of
    /// another workspace may have taken it over since. A host that cannot
    /// close places leaves that to the place itself.
    pub(crate) async fn close_recorded_place(
        &self,
        workspace: &str,
        host: HostName,
        place: &str,
    ) -> Result<PlaceClose> {
        let _ownership = self.place_ownership.lock().await;
        let Some((owner, origin)) = self.place_owner(host, place).await? else {
            return Ok(PlaceClose::NotRecorded);
        };
        if owner.as_deref() != Some(workspace) {
            return Ok(PlaceClose::OwnedElsewhere);
        }
        if let Some(host) = self.session_hosts.iter().find(|known| known.name() == host) {
            host.close(
                &self.paths.state,
                &origin,
                &Owner::Workspace(workspace.to_owned()),
            )
            .await?;
        }
        self.forget_owned_place(origin).await?;
        Ok(PlaceClose::Closed)
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
            .find(|host| host.name() == close.place.host)
            .with_context(|| format!("terminal host {} is unavailable", close.place.host))?;
        host.close(&self.paths.state, &close.place, &close.owner)
            .await?;
        self.store
            .run(move |db| pending::forget(db, &close.id))
            .await
    }

    /// Close the session a host labelled as `owner`'s.
    async fn close_session(&self, host: &dyn crate::sessions::Host, name: &str, owner: Owner) {
        let place = Origin::created(host.name(), name.to_owned());
        if let Err(error) = host.close(&self.paths.state, &place, &owner).await {
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
