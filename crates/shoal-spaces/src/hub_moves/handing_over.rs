//! The start a package of a space places it by when the hub continues the
//! space on another machine, as when its own shuts down or sleeps.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result};
use shoal_ids::member_ids::MemberId;
use shoal_schedule::placement::snapshot::{AgentStart, Request};

use crate::{
    config::{Effective, modes::ModeName},
    launches::{heavy, lifetime},
    model::{RemovedSpace, Space},
};

impl Spaces {
    /// The origin of its repository, the tool of the agent that continues
    /// there, else its settings' default one, its mode and its lifetime.
    pub async fn package_start(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        packaged: &RemovedSpace,
    ) -> Result<Request> {
        let (origin, settings) = self.handover_settings(daemon, space).await?;
        let stopped = packaged.agents.iter().map(|agent| &agent.record.agent);
        let last = packaged.last_sessions.iter().map(|last| &last.agent);
        let agent = (stopped.chain(last).next().cloned())
            .or_else(|| settings.agent.default.clone())
            .context("it saved no agent and its settings name no default agent")?;
        let mode = space.mode.clone().unwrap_or(ModeName::DEFAULT);
        Ok(Request {
            origin,
            base: None,
            agent: Some(AgentStart {
                agent: agent.name().to_owned(),
                mode: mode.to_string(),
            }),
            heavy: heavy(&settings, &mode),
            lifetime: lifetime(space.worktree().is_some_and(|worktree| worktree.disposable)),
            // The hub, which places it, names its caller.
            caller: MemberId::from(String::new()),
            parent: None,
        })
    }

    /// By the key machines match repositories by, with the settings its agent
    /// starts under: none and the global config's for a chat without one.
    async fn handover_settings(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<(Option<String>, Effective)> {
        let Some(id) = &space.repository_id else {
            return Ok((None, self.config.current().resolve(&Default::default())?));
        };
        let repository = daemon
            .repository(crate::repository::Selector::from(id))
            .await?;
        let url = crate::repository::remote_url_from_source(&repository.source).await?;
        let settings = daemon.repository_settings(id).await?;
        let origin = url.map(|url| shoal_protocol::repository::url_key(&url));
        Ok((origin, settings))
    }
}
