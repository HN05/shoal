//! End terminal sessions Shoal created once their owner ends, including
//! leftovers from failed closes. An owner is a space, a workspace or a chat,
//! whose removal ends its places the same way whatever its kind. Hosts
//! supply discovery and close operations, not lifecycle policy. Sessions a
//! host labels as Shoal's are found there; places a host does not label are
//! closed from the daemon's records of the places Shoal created.
pub(super) mod pending;

use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use anyhow::{Context as _, Result};

use super::Manager;
use crate::{
    daemon::notifications::NotificationKind,
    sessions::{HostName, Origin, Owner, PlaceClose},
};

impl Manager {
    /// End the places of the space `id`, named `name`, which every removal
    /// runs once it committed; failures are visible and the next orphan
    /// sweep retries them without restoring the space.
    pub(crate) async fn close_space_places(&self, id: &SpaceId, name: &SpaceName) {
        for host in &self.session_hosts {
            if let Err(error) = self.close_host_space_sessions(host.as_ref(), id).await {
                self.session_cleanup_failed(Some(name), &error).await;
            }
        }
        self.close_owned_places(Some(id.clone())).await;
    }

    async fn close_host_space_sessions(
        &self,
        host: &dyn crate::sessions::Host,
        space: &SpaceId,
    ) -> Result<()> {
        for session in host.sessions(&self.paths.state).await? {
            if session.space == *space {
                self.close_session(host, &session.name, session.space).await;
            }
        }
        Ok(())
    }

    /// Whether `host` labels its session `place` as one Shoal created for
    /// `space`, which the space's removal closes.
    pub(super) async fn labels_session(
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
            if !self.space_exists(&session.space).await? {
                self.close_session(host, &session.name, session.space).await;
            }
        }
        Ok(())
    }

    /// Close the places Shoal created for `space`, which removal ended, or
    /// for every space whose record is gone.
    async fn close_owned_places(&self, space: Option<SpaceId>) {
        let sweep = space.is_none();
        let places = match self.owned_places(space).await {
            Ok(places) => places,
            Err(error) => return self.session_cleanup_failed(None, &error).await,
        };
        for (owner, place) in places {
            let removed = !sweep || matches!(self.space_exists(&owner).await, Ok(false));
            if removed && let Err(error) = self.close_recorded_place(&owner, &place).await {
                self.session_cleanup_failed(None, &error).await;
            }
        }
    }

    /// Close `place`, recorded as one Shoal created, and forget it while
    /// its record still names `space` its owner: an agent of another space
    /// or a chat may have taken it over since. A host the daemon cannot
    /// close places of leaves that to the place's watcher.
    pub(crate) async fn close_recorded_place(
        &self,
        space: &SpaceId,
        place: &Origin,
    ) -> Result<PlaceClose> {
        let _ownership = self.place_ownership.lock().await;
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

    /// Close the session a host labelled as `space`'s, unless another space
    /// took it over: a chat or agent that starts in a session is recorded
    /// there, and records the session, before it relabels it. Both are read
    /// under the lock takeovers hold.
    async fn close_session(&self, host: &dyn crate::sessions::Host, name: &str, space: SpaceId) {
        let place = Origin::created(host.name(), name.to_owned());
        let _ownership = self.place_ownership.lock().await;
        if let Err(error) = self.close_labelled(host, &place, space).await {
            self.session_cleanup_failed(None, &error).await;
        }
    }

    /// [`Self::close_session`] once its caller holds the ownership lock.
    async fn close_labelled(
        &self,
        host: &dyn crate::sessions::Host,
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

    async fn session_cleanup_failed(&self, workspace: Option<&SpaceName>, error: &anyhow::Error) {
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
