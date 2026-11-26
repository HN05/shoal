//! Ending the terminal sessions Shoal created once their owner ends or its
//! agent left them, including leftovers from failed closes. Hosts supply
//! discovery and close operations, not lifecycle policy.
pub mod left;
pub mod pending;

use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use crate::Agents;
use crate::daemon::Daemon;
use shoal_core::sessions::HostName;
use shoal_core::sessions::Origin;
use shoal_core::sessions::Owner;
use shoal_core::sessions::PlaceClose;
use shoal_daemon_base::alerts::AlertKind;

impl Agents {
    /// End the places of the space `id`, named `name`, which every removal
    /// runs once it committed; failures are visible and the next orphan
    /// sweep retries them without restoring the space.
    pub async fn close_space_places(&self, daemon: &dyn Daemon, id: &SpaceId, name: &SpaceName) {
        for host in &self.session_hosts {
            if let Err(error) = self
                .close_host_space_sessions(daemon, host.as_ref(), id)
                .await
            {
                self.session_cleanup_failed(daemon, Some((name, id)), &error)
                    .await;
            }
        }
        self.close_owned_places(daemon, Some(id.clone())).await;
    }

    async fn close_host_space_sessions(
        &self,
        daemon: &dyn Daemon,
        host: &dyn shoal_core::sessions::Host,
        space: &SpaceId,
    ) -> Result<()> {
        for session in host.sessions(&self.paths.state).await? {
            if session.space == *space {
                self.close_session(daemon, host, &session.name, session.space)
                    .await;
            }
        }
        Ok(())
    }

    /// Whether `host` labels its session `place` as one Shoal created for
    /// `space`, which the space's removal closes.
    pub async fn labels_session(
        &self,
        host: HostName,
        place: &str,
        space: &SpaceId,
    ) -> Result<bool> {
        let Some(host) = self.session_hosts.iter().find(|known| known.name() == host) else {
            return Ok(false);
        };
        let sessions = host.sessions(&self.paths.state).await?;
        Ok(sessions
            .iter()
            .any(|session| session.name == place && session.space == *space))
    }

    /// Read ownership fresh for each session: a stale list must not classify a
    /// newly registered workspace as removed.
    pub async fn cleanup_orphan_sessions(&self, daemon: &dyn Daemon) {
        self.retry_session_closes(daemon, None).await;
        for host in &self.session_hosts {
            if let Err(error) = self.cleanup_host_sessions(daemon, host.as_ref()).await {
                self.session_cleanup_failed(daemon, None, &error).await;
            }
        }
        self.close_owned_places(daemon, None).await;
    }

    async fn cleanup_host_sessions(
        &self,
        daemon: &dyn Daemon,
        host: &dyn shoal_core::sessions::Host,
    ) -> Result<()> {
        for session in host.sessions(&self.paths.state).await? {
            if !daemon.space_exists(&session.space).await? {
                self.close_session(daemon, host, &session.name, session.space)
                    .await;
            }
        }
        Ok(())
    }

    /// Close the places Shoal created for `space`, which removal ended, or
    /// for every space whose record is gone.
    pub async fn close_owned_places(&self, daemon: &dyn Daemon, space: Option<SpaceId>) {
        let sweep = space.is_none();
        let places = match self.owned_places(space).await {
            Ok(places) => places,
            Err(error) => return self.session_cleanup_failed(daemon, None, &error).await,
        };
        for (owner, place) in places {
            let removed = !sweep || matches!(daemon.space_exists(&owner).await, Ok(false));
            if removed && let Err(error) = self.close_recorded_place(&owner, &place).await {
                self.session_cleanup_failed(daemon, None, &error).await;
            }
        }
    }

    /// Close `place`, recorded as one Shoal created, and forget it while
    /// its record still names `space` its owner: an agent of another space
    /// or a chat may have taken it over since. A host the daemon cannot
    /// close places of leaves that to the place's watcher.
    pub async fn close_recorded_place(
        &self,
        space: &SpaceId,
        place: &Origin,
    ) -> Result<PlaceClose> {
        let _ownership = self.place_ownership.lock().await;
        self.close_recorded(space, place).await
    }

    /// [`Self::close_recorded_place`] once its caller holds the ownership
    /// lock.
    async fn close_recorded(&self, space: &SpaceId, place: &Origin) -> Result<PlaceClose> {
        if self.place_runs_elsewhere(place, space).await? {
            return Ok(PlaceClose::OwnedElsewhere);
        }
        let Some((owner, origin)) = self.place_owner(place).await? else {
            return self.close_labelled_session(space, place).await;
        };
        if owner != *space {
            return Ok(PlaceClose::OwnedElsewhere);
        }
        let host = self
            .session_hosts
            .iter()
            .find(|known| known.name() == place.host);
        let Some(host) = host else {
            self.forget_owned_place(origin).await?;
            return Ok(PlaceClose::HostCloses);
        };
        host.close(&self.paths.state, &origin, &Owner::Space(space.to_owned()))
            .await?;
        self.forget_owned_place(origin).await?;
        Ok(PlaceClose::Closed)
    }

    /// Close `place`, which the daemon keeps no record of, when its host
    /// labels it as `space`'s session, as zmx does a session from before
    /// Shoal recorded them; the host's close verifies the labels again.
    async fn close_labelled_session(&self, space: &SpaceId, place: &Origin) -> Result<PlaceClose> {
        if !self.labels_session(place.host, place.id(), space).await? {
            return Ok(PlaceClose::NotRecorded);
        }
        let host = self
            .session_hosts
            .iter()
            .find(|known| known.name() == place.host)
            .context("the host that labels the session is gone")?;
        host.close(&self.paths.state, place, &Owner::Space(space.to_owned()))
            .await?;
        Ok(PlaceClose::Closed)
    }

    /// The close was queued atomically with deleting the verified execution.
    pub async fn close_demo_session(&self, daemon: &dyn Daemon, id: Option<String>) {
        if id.is_some() {
            self.retry_session_closes(daemon, id).await;
        }
    }

    async fn retry_session_closes(&self, daemon: &dyn Daemon, id: Option<String>) {
        let pending = self
            .store
            .run(move |db| pending::list(db, id.as_deref()))
            .await;
        match pending {
            Ok(pending) => {
                for close in pending {
                    if let Err(error) = self.close_pending_session(close).await {
                        self.session_cleanup_failed(daemon, None, &error).await;
                    }
                }
            }
            Err(error) => self.session_cleanup_failed(daemon, None, &error).await,
        }
    }

    /// Close `close`'s place, and the session nested in it, which outlives
    /// a closed place, while `close`'s owner still owns each.
    async fn close_pending_session(&self, close: pending::Close) -> Result<()> {
        for place in close.place.with_nested() {
            let host = self
                .session_hosts
                .iter()
                .find(|host| host.name() == place.host)
                .with_context(|| format!("terminal host {} is unavailable", place.host))?;
            host.close(&self.paths.state, place, &close.owner).await?;
        }
        self.store
            .run(move |db| pending::forget(db, &close.id))
            .await
    }

    /// Close the session a host labelled as `space`'s, unless another space
    /// took it over: a chat or agent that starts in a session is recorded
    /// there, and records the session, before it relabels it. Both are read
    /// under the lock takeovers hold.
    async fn close_session(
        &self,
        daemon: &dyn Daemon,
        host: &dyn shoal_core::sessions::Host,
        name: &str,
        space: SpaceId,
    ) {
        let place = Origin::created(host.name(), name.to_owned());
        let _ownership = self.place_ownership.lock().await;
        if let Err(error) = self.close_labelled(host, &place, space).await {
            self.session_cleanup_failed(daemon, None, &error).await;
        }
    }

    /// [`Self::close_session`] once its caller holds the ownership lock.
    async fn close_labelled(
        &self,
        host: &dyn shoal_core::sessions::Host,
        place: &Origin,
        space: SpaceId,
    ) -> Result<()> {
        let recorded_elsewhere =
            (self.place_owner(place).await?).is_some_and(|(owner, _)| owner != space);
        if recorded_elsewhere || self.place_runs_elsewhere(place, &space).await? {
            return Ok(());
        }
        host.close(&self.paths.state, place, &Owner::Space(space))
            .await
    }

    async fn session_cleanup_failed(
        &self,
        daemon: &dyn Daemon,
        workspace: Option<(&SpaceName, &SpaceId)>,
        error: &anyhow::Error,
    ) {
        let message =
            format!("terminal session cleanup failed: {error:#}; retry with shoal cleanup");
        match workspace {
            Some((name, id)) => {
                (daemon.notify_space(name, id, AlertKind::CleanupFailed, message)).await
            }
            None => daemon.notify(None, AlertKind::CleanupFailed, message).await,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
