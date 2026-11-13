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

impl AgentLaunch {
    /// The agent named, if any.
    pub(super) fn named(self) -> Option<Agent> {
        match self {
            Self::Explicit(agent) | Self::Add(agent) | Self::IssueDefault(agent) => agent,
        }
    }
}

/// The effective settings of the repository a workspace is added in, or of
/// the existing workspace a launch goes to, loaded on first use.
pub(super) struct AddSettings<'a> {
    ctx: &'a Context,
    target: ConfigTarget,
    effective: tokio::sync::OnceCell<crate::config::Effective>,
}

impl<'a> AddSettings<'a> {
    pub(super) fn new(ctx: &'a Context, repository: &crate::repository::Selector) -> Self {
        Self::of(ctx, ConfigTarget::Repository(repository.into()))
    }

    /// The settings of the existing workspace `id`.
    pub(super) fn workspace(ctx: &'a Context, id: &crate::client::space_ids::SpaceId) -> Self {
        Self::of(ctx, ConfigTarget::Workspace(id.into()))
    }

    fn of(ctx: &'a Context, target: ConfigTarget) -> Self {
        Self {
            ctx,
            target,
            effective: tokio::sync::OnceCell::new(),
        }
    }

    pub(super) async fn get(&self) -> Result<&crate::config::Effective> {
        self.effective
            .get_or_try_init(|| crate::config::settings(&self.ctx.paths, self.target.clone()))
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
        crate::agents::ensure_available(agent, settings.get().await?)?;
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
    let resolved = crate::agents::resolve_model(settings, agent, model.clone());
    crate::agents::check_model(settings, agent, &resolved)
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

/// What a launch in an added workspace tells its agent: the `opening`, in
/// its mode with `narrowing` applied, the agent's `args`, and the
/// session it starts from a fork of.
pub(super) struct Launch {
    pub opening: agents::Opening,
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub args: Vec<OsString>,
    pub fork: Option<crate::client::agent::AgentSessionId>,
}

/// Start `resolved`'s agent in `workspace` as `launch` says, else open a
/// shell there.
pub(super) async fn launch(
    ctx: &Context,
    resolved: ResolvedAddAgent,
    workspace: Workspace,
    launch: Launch,
) -> Result<i32> {
    let Some(agent) = resolved.agent else {
        return terminal::open_shell(ctx, &workspace.path).await;
    };
    let options = agents::LaunchOptions {
        fork: launch.fork,
        model: agents::ModelRequest::Override(resolved.model),
        narrowing: launch.narrowing,
        ..Default::default()
    };
    agents::launch_agent(ctx, agent, workspace, launch.opening, launch.args, options).await
}
