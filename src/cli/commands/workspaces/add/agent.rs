//! The agent an addition starts: named, configured or picked.
use std::ffi::OsString;

use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context, terminal},
    model::Workspace,
    protocol::ConfigTarget,
    start_plan::{Creation, ResolvedAddAgent},
};

pub(in crate::cli::commands) enum AgentLaunch {
    Explicit(Option<Agent>),
    /// An ordinary add uses the interactive picker when no agent is named.
    Add(Option<Agent>),
    IssueDefault(Option<Agent>),
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
                crate::client::workspaces::settings(
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

/// No agent for a fork PR's head, which a workspace at a commit holds,
/// refusing one that was named.
pub(super) fn for_creation(creation: &Creation, agent: AgentChoice) -> Result<AgentChoice> {
    if creation.pull_head.is_none() {
        return Ok(agent);
    }
    anyhow::ensure!(
        !matches!(agent, AgentChoice::Chosen(Some(_))),
        "the PR comes from a fork, so its head opens in a workspace at a commit, which holds no agent of its own"
    );
    Ok(AgentChoice::Chosen(None))
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
            match agent.or(settings.get().await?.agent.default.clone()) {
                Some(agent) => Some(agent),
                None => return Ok(AgentChoice::Picker),
            }
        }
    };
    if let Some(agent) = &agent {
        crate::agents::ensure_available(agent.name(), settings.get().await?)?;
    }
    Ok(AgentChoice::Chosen(agent))
}

/// Refuse `model` for an agent already chosen before any prompt asks for the
/// rest of the addition; a picked agent is checked once picked.
pub(super) async fn check_chosen_model(
    settings: &AddSettings<'_>,
    agent: &AgentChoice,
    model: &ModelChoice,
) -> Result<()> {
    if let AgentChoice::Chosen(Some(agent)) = agent {
        check_model(settings.get().await?, agent, model)?;
    }
    Ok(())
}

/// Refuse `model` for `agent` when its launch cannot pass it on.
fn check_model(
    settings: &crate::config::Effective,
    agent: &Agent,
    model: &ModelChoice,
) -> Result<()> {
    let resolved = crate::agents::resolve_model(settings, agent.name(), model.clone());
    crate::agents::check_model(settings, agent.name(), &resolved)
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

/// The agent an addition starts, refusing a `model` it cannot run with
/// before anything is created.
pub(super) async fn resolve_add_agent(
    ctx: &Context,
    settings: &AddSettings<'_>,
    agent: AgentChoice,
    model: ModelChoice,
) -> Result<ResolvedAddAgent> {
    let agent = match agent {
        AgentChoice::Chosen(agent) => agent,
        AgentChoice::Picker => agents::pick_default_agent(ctx, settings.get().await?)?,
        AgentChoice::PickerIfInteractive if ctx.interactive() => {
            agents::pick_default_agent(ctx, settings.get().await?)?
        }
        AgentChoice::PickerIfInteractive => None,
    };
    match &agent {
        Some(agent) => check_model(settings.get().await?, agent, &model)?,
        None => anyhow::ensure!(
            model.is_empty(),
            "--model and --effort need an agent; name one with --agent"
        ),
    }
    Ok(ResolvedAddAgent { agent, model })
}

/// Start `resolved`'s agent in `workspace` told `opening`, from a fork of
/// session `fork` when given, else open a shell there.
pub(super) async fn launch(
    ctx: &Context,
    resolved: ResolvedAddAgent,
    workspace: Workspace,
    opening: agents::Opening,
    args: Vec<OsString>,
    fork: Option<String>,
) -> Result<i32> {
    let Some(agent) = resolved.agent else {
        return terminal::open_shell(ctx, &workspace.path).await;
    };
    let options = agents::LaunchOptions {
        fork,
        model: agents::ModelRequest::Override(resolved.model),
        ..Default::default()
    };
    agents::launch_agent(ctx, agent, workspace, opening, args, options).await
}
