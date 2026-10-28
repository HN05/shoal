//! The agent an addition starts: named, configured or picked.
use std::ffi::OsString;

use anyhow::Result;

use crate::{
    agent::Agent,
    cli::{agents, client, commands::issues, context::Context, terminal},
    model::Workspace,
    protocol::ConfigTarget,
};

pub(in crate::cli::commands) enum AgentLaunch {
    Explicit(Option<Agent>),
    /// An ordinary add uses the interactive picker when no agent is named.
    Add(Option<Agent>),
    IssueDefault(Option<Agent>),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(from = "StoredAddAgent")]
pub(super) struct ResolvedAddAgent {
    pub(super) agent: Option<Agent>,
    /// Whether the agent runs tracked, never handed to a desktop app.
    pub(super) tracked: bool,
}

/// A plan's agent as queued entries store it, including plans queued before
/// `tracked`, which recorded Codex's mode instead.
#[derive(serde::Deserialize)]
struct StoredAddAgent {
    agent: Option<Agent>,
    #[serde(default)]
    tracked: bool,
    #[serde(default)]
    codex_mode: Option<crate::agent::AgentMode>,
}

/// Codex's CLI meant a tracked launch.
impl From<StoredAddAgent> for ResolvedAddAgent {
    fn from(stored: StoredAddAgent) -> Self {
        Self {
            agent: stored.agent,
            tracked: stored.tracked || stored.codex_mode == Some(crate::agent::AgentMode::Cli),
        }
    }
}

/// The repository's effective settings, loaded on first use.
pub(super) struct AddSettings<'a> {
    ctx: &'a Context,
    repository: &'a crate::repository::Selector,
    effective: tokio::sync::OnceCell<crate::config::Effective>,
}

impl<'a> AddSettings<'a> {
    pub(super) fn new(ctx: &'a Context, repository: &'a crate::repository::Selector) -> Self {
        Self {
            ctx,
            repository,
            effective: tokio::sync::OnceCell::new(),
        }
    }

    pub(super) async fn get(&self) -> Result<&crate::config::Effective> {
        self.effective
            .get_or_try_init(|| {
                client::settings(
                    &self.ctx.paths,
                    ConfigTarget::Repository(self.repository.into()),
                )
            })
            .await
    }
}

pub(super) enum AgentChoice {
    Chosen(Option<Agent>),
    /// No agent was named or configured; the picker runs after lookups.
    Picker,
    /// Ordinary additions pick only when attached to a terminal.
    PickerIfInteractive,
}

/// Validate a named agent before looking up the issue or creating work.
pub(super) async fn choose_add_agent(
    settings: &AddSettings<'_>,
    agent: AgentLaunch,
) -> Result<AgentChoice> {
    let agent = match agent {
        AgentLaunch::Explicit(agent) => agent,
        AgentLaunch::Add(agent) => {
            if agent.is_none() {
                return Ok(AgentChoice::PickerIfInteractive);
            }
            agent
        }
        AgentLaunch::IssueDefault(agent) => {
            match agent.or(settings.get().await?.default_agent.clone()) {
                Some(agent) => Some(agent),
                None => return Ok(AgentChoice::Picker),
            }
        }
    };
    if let Some(agent) = &agent {
        agents::ensure_available(agent.name(), settings.get().await?)?;
    }
    Ok(AgentChoice::Chosen(agent))
}

/// A picked issue defaults its agent as `--issue` does.
pub(super) async fn choose_for_picked_issue(
    settings: &AddSettings<'_>,
    agent: AgentChoice,
    picked: bool,
) -> Result<AgentChoice> {
    match agent {
        AgentChoice::PickerIfInteractive if picked => {
            choose_add_agent(settings, AgentLaunch::IssueDefault(None)).await
        }
        agent => Ok(agent),
    }
}

pub(super) async fn resolve_add_agent(
    ctx: &Context,
    settings: &AddSettings<'_>,
    agent: AgentChoice,
) -> Result<ResolvedAddAgent> {
    let agent = match agent {
        AgentChoice::Chosen(agent) => agent,
        AgentChoice::Picker => agents::pick_default_agent(ctx, settings.get().await?)?,
        AgentChoice::PickerIfInteractive if ctx.interactive() => {
            agents::pick_default_agent(ctx, settings.get().await?)?
        }
        AgentChoice::PickerIfInteractive => None,
    };
    Ok(ResolvedAddAgent {
        agent,
        tracked: false,
    })
}

impl ResolvedAddAgent {
    pub(super) async fn launch(
        self,
        ctx: &Context,
        workspace: Workspace,
        issue: Option<issues::Issue>,
        args: Vec<OsString>,
    ) -> Result<i32> {
        let Some(agent) = self.agent else {
            return terminal::open_shell(ctx, &workspace.path).await;
        };
        let prompt = issue.map(|issue| issue.prompt());
        let options = agents::LaunchOptions {
            tracked: self.tracked,
            ..Default::default()
        };
        agents::launch_agent(ctx, agent, workspace, prompt, args, options).await
    }
}

#[cfg(test)]
mod tests;
