//! The start a package of a space places it by when the hub continues the
//! space on another machine, as when its own shuts down or sleeps: the hub
//! continues it on the machine its placement puts that start on.
use anyhow::{Context as _, Result};
use shoal_ids::member_ids::MemberId;
use shoal_schedule::placement::snapshot::Request;

use crate::{
    config::{Effective, modes::ModeName},
    daemon::{launches::heavy, manager::Manager},
    model::{RemovedSpace, Space},
};

impl Manager {
    /// How placement reads a start of the agent of `space`, which a
    /// package that records it as `packaged` continues elsewhere: the
    /// origin of its repository, the tool of the agent that continues
    /// there, else its settings' default one, and its mode.
    pub(crate) async fn package_start(
        &self,
        space: &Space,
        packaged: &RemovedSpace,
    ) -> Result<Request> {
        let (origin, settings) = self.handover_settings(space).await?;
        let stopped = packaged.agents.iter().map(|agent| &agent.record.agent);
        let last = packaged.last_sessions.iter().map(|last| &last.agent);
        let agent = (stopped.chain(last).next().cloned())
            .or_else(|| settings.agent.default.clone())
            .context("it saved no agent and its settings name no default agent")?;
        let mode = space.mode.clone().unwrap_or(ModeName::DEFAULT);
        Ok(Request {
            origin,
            base: None,
            agent: agent.name().to_owned(),
            heavy: heavy(&settings, &mode),
            mode: mode.to_string(),
            // The hub, which places it, names its caller.
            caller: MemberId::from(String::new()),
            parent: None,
        })
    }

    /// The origin of the repository of `space`, by the key machines match
    /// repositories by, with the settings its agent starts under: none and
    /// the global config's for a chat without one.
    async fn handover_settings(&self, space: &Space) -> Result<(Option<String>, Effective)> {
        let Some(id) = &space.repository_id else {
            return Ok((None, self.config().resolve(&Default::default())?));
        };
        let repository = self.repositories.repository(id).await?;
        let url = crate::repository::remote_url_from_source(&repository.source).await?;
        let settings = self.repositories.repository_settings(id).await?;
        let origin = url.map(|url| shoal_protocol::repository::url_key(&url));
        Ok((origin, settings))
    }
}
