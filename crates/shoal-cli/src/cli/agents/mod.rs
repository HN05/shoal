//! The CLI's side of the agents Shoal launches (see [`crate::agents`]):
//! picking one, `shoal run <name>`, launching one from a command in a
//! workspace or a chat, handing a workspace to a desktop app, and the trust
//! and session files those launches prepare.
mod desktop;
mod detached;
mod environment;
mod launch;
mod sessions;
mod trust;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, ModelChoice},
    agents::{
        ADDED, Added, AgentHome, HomeRecords, agent_integration, ensure_available, ensure_forks,
        resolve_model,
    },
    cli::{context::Context, ui},
    config::{Effective, modes, templates},
    model::{RemovedSpace, Workspace},
    protocol::ConfigTarget,
    tools::ensure_program,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli) use crate::agents::{installed, names};
pub(in crate::cli) use desktop::{app, ensure_opens, hand_off};
pub(in crate::cli) use sessions::{SessionHome, install_kept_session_files, place_forked_sessions};

/// One launch of an agent in a space, a ready workspace or a chat; settings
/// are never cached across launches.
pub(in crate::cli) struct Launch {
    pub name: Agent,
    /// The space's records as the daemon has them now.
    pub space: HomeRecords,
    pub settings: Effective,
    /// Shoal's guidance and the mode's rules.
    pub instructions: String,
    /// The first message, such as an issue with the mode's instructions.
    pub prompt: Option<String>,
    pub args: Vec<OsString>,
    /// The arguments that start the agent from a fork of a session, which
    /// lead the others as a subcommand does.
    pub fork: Vec<OsString>,
    /// The model and effort it runs with.
    pub model: ModelChoice,
    /// The mode it starts in.
    pub mode: modes::ModeName,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// Recovery records of stopped agents this one replaces, consumed once
    /// it has started.
    pub replaces: Vec<PathBuf>,
    /// Whether it begins a new session, rather than continuing or forking one.
    pub new_session: bool,
}

/// What an agent's launch is told: its mode decides the rules of its
/// system text and the instructions after its subject.
pub(in crate::cli) enum Opening {
    /// A new session, starting on `subject` when it has one.
    New {
        mode: modes::ModeName,
        subject: Option<templates::Subject>,
    },
    /// A session in `mode` told `message` as it is, such as one that
    /// replaces a stopped session.
    Told {
        mode: modes::ModeName,
        message: String,
    },
}

impl Opening {
    fn mode(&self) -> &modes::ModeName {
        match self {
            Self::New { mode, .. } | Self::Told { mode, .. } => mode,
        }
    }

    /// Whether a launch with `options` begins a new session: one that
    /// neither replaces a stopped session nor starts from a fork.
    fn begins_session(&self, options: &LaunchOptions) -> bool {
        matches!(self, Self::New { .. }) && options.fork.is_none() && options.replaces.is_empty()
    }

    /// The system text and first message of agent `name`'s launch in
    /// `home`; one told a message as it is takes the rules with the fields
    /// of the issue its space links.
    fn brief(
        &self,
        settings: &Effective,
        name: &Agent,
        home: AgentHome<'_>,
    ) -> Result<templates::Brief> {
        match self {
            Self::New { mode, subject } => {
                templates::brief(settings, mode, home.place(settings, name), subject.as_ref())
            }
            Self::Told { mode, message } => Ok(templates::Brief {
                system: home.system(settings, name, mode)?,
                first_message: Some(message.clone()),
            }),
        }
    }
}

/// `brief` as an agent in `home` receives it: a chat's agent that takes no
/// instructions apart from its first message, as `takes_instructions` says,
/// gets them there ahead of its subject, or else of the request to wait for
/// the user, since they start a turn before the user speaks.
fn delivered(
    brief: templates::Brief,
    home: AgentHome<'_>,
    takes_instructions: bool,
) -> templates::Brief {
    if takes_instructions || matches!(home, AgentHome::Workspace(_)) {
        return brief;
    }
    let templates::Brief {
        system,
        first_message,
    } = brief;
    templates::Brief {
        system: templates::chat_first_message(&system, first_message.as_deref())
            .unwrap_or_default(),
        first_message: None,
    }
}

/// How a launch relates to the agents around it.
#[derive(Default)]
pub(in crate::cli) struct LaunchOptions {
    /// As [`Launch::replaces`].
    pub replaces: Vec<PathBuf>,
    /// The session the agent starts from a fork of, which stays unchanged.
    pub fork: Option<shoal_ids::agent_session_ids::AgentSessionId>,
    /// The model and effort the launch asks for.
    pub model: ModelRequest,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
}

/// The model and effort a launch asks for.
pub(in crate::cli) enum ModelRequest {
    /// Values over the configured ones, which fill in the rest.
    Override(ModelChoice),
    /// What a restored agent ran with, used as it is; a value it left to
    /// the tool stays the tool's.
    Kept(ModelChoice),
}

impl Default for ModelRequest {
    fn default() -> Self {
        Self::Override(ModelChoice::default())
    }
}

/// Whether a launch of `agent` under `settings` runs in a session this
/// terminal attaches to, rather than in place or the agent's own way.
pub(in crate::cli) fn runs_in_session(agent: &Agent, settings: &Effective) -> bool {
    !runs_its_own_way(agent, settings) && crate::terminal::keeps_agents_in_sessions(settings)
}

/// Whether a launch of `agent` under `settings` leaves the launching process
/// at once, as one its integration runs detached, such as Happy, does.
pub(in crate::cli) fn runs_its_own_way(agent: &Agent, settings: &Effective) -> bool {
    agent_integration(settings, agent).detached().is_some()
}

/// Start an agent in `space`, a ready workspace or a chat as its records
/// say, giving it the prompt the way it accepts one.
pub(in crate::cli) async fn launch_agent(
    ctx: &Context,
    agent: Agent,
    space: HomeRecords,
    opening: Opening,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let home = space.home()?;
    let target = ConfigTarget::Space(SpaceSelector::id(home.id()));
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    let integration = agent_integration(&settings, &agent);
    let fork = match &options.fork {
        Some(session) => {
            sessions::find_forked_session(ctx, &space, (&settings, &agent), session).await;
            fork_args(&settings, &agent, session, home.path())?
        }
        None => Vec::new(),
    };
    let takes_instructions = match integration.detached() {
        Some(app) => detached::takes_instructions(app, &settings),
        None => settings.launch(&agent).arguments.instructions.is_some(),
    };
    let brief = opening.brief(&settings, &agent, home)?;
    let templates::Brief {
        system: instructions,
        first_message: prompt,
    } = delivered(brief, home, takes_instructions);
    let new_session = opening.begins_session(&options);
    let model = match options.model {
        ModelRequest::Override(requested) => resolve_model(&settings, &agent, requested),
        ModelRequest::Kept(model) => model,
    };
    let launch = Launch {
        name: agent,
        space,
        settings,
        instructions,
        prompt,
        args,
        fork,
        model,
        mode: opening.mode().clone(),
        narrowing: options.narrowing,
        replaces: options.replaces,
        new_session,
    };
    let replaces = launch.replaces.clone();
    let Some(detached) = integration.detached() else {
        return launch::tracked(ctx, integration, launch).await;
    };
    let code = detached::start(ctx, integration, detached, launch).await?;
    // A launch of its own returns once its agent has started.
    for record in &replaces {
        crate::execution::recovery::consume(record)?;
    }
    Ok(code)
}

/// The agent a start runs under `settings`: the one `named`, else
/// `agent.default`, else the one a picker picks on a terminal. Without
/// either, a start off a terminal names none, which the daemon refuses.
pub(in crate::cli) fn start_agent(
    ctx: &Context,
    settings: &Effective,
    named: Option<Agent>,
) -> Result<Option<Agent>> {
    match named.or(settings.agent.default.clone()) {
        Some(agent) => {
            ensure_available(&agent, settings)?;
            Ok(Some(agent))
        }
        None if ctx.interactive() => pick_installed_agent(ctx, settings),
        None => Ok(None),
    }
}

/// The picker standing in for a missing `agent.default`: the agents whose
/// programs are installed.
fn pick_installed_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let installed = |name: &Agent| installed(name, &settings.launchers, &search_path);
    pick_agent(ctx, settings, installed, None)
}

/// The picker standing in for a missing reviewer: the agents with a review
/// template whose review command is installed, or “No agent”, which runs
/// no review.
pub(in crate::cli) fn pick_reviewer(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    let available = |name: &Agent| {
        settings.launch(name).arguments.review.is_some()
            && ensure_reviewer_available(name, settings).is_ok()
    };
    pick_agent(ctx, settings, available, Some("No agent"))
}

/// A picker of the agents `available` accepts, with the choice of none
/// under the label `none` when given.
fn pick_agent(
    ctx: &Context,
    settings: &Effective,
    available: impl Fn(&Agent) -> bool,
    none: Option<&'static str>,
) -> Result<Option<Agent>> {
    ensure!(ctx.interactive(), crate::agents::NO_AGENT);
    let agents = names(settings);
    let mut choices: Vec<_> = agents
        .iter()
        .filter(|agent| available(agent))
        .map(|agent| (Some(agent.clone()), agent.name()))
        .collect();
    // A picker cannot run without fzf, which leaves the agent unpicked.
    if choices.is_empty() || !ui::can_pick(ctx) {
        return Ok(None);
    }
    if let Some(none) = none {
        choices.push((None, none));
    }
    let picked: Option<Agent> = ui::pick_choice(ctx, "Agent> ", &choices)?;
    // The picker offers agents by their own programs; the launch may need
    // a session host too, refused before anything is created.
    if let Some(agent) = &picked {
        ensure_available(agent, settings)?;
    }
    Ok(picked)
}

/// Agent names Shoal knows without configuration, for help and completion.
pub(in crate::cli) fn builtin_names() -> Vec<String> {
    let mut names: Vec<String> = crate::ai::PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .collect();
    for Added { name, .. } in ADDED {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    names
}

/// Refuse a reviewer without a review template or whose review command's
/// program is missing. A review runs without a session, so it needs no
/// terminal host.
pub(in crate::cli) fn ensure_reviewer_available(name: &Agent, settings: &Effective) -> Result<()> {
    settings.check_reviewer(name)?;
    let program = settings
        .launchers
        .get(name.name())
        .and_then(|argv| argv.first())
        .with_context(|| {
            format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
        })?;
    ensure_program(name.name(), program)
}

/// The arguments that start agent `name` in `directory` from a fork of
/// `session`, refusing an agent [`ensure_forks`] refuses.
pub(in crate::cli) fn fork_args(
    settings: &Effective,
    name: &Agent,
    session: &shoal_ids::agent_session_ids::AgentSessionId,
    directory: &Path,
) -> Result<Vec<OsString>> {
    ensure_forks(settings, name)?;
    let directory = directory
        .to_str()
        .context("cannot fork a session in a directory whose path is not UTF-8")?;
    Ok(settings
        .launch(name)
        .arguments
        .fork_args(session, directory)
        .unwrap_or_default())
}

/// The key that cancels the running turn of agent `name` under `settings`,
/// unless Shoal knows none.
pub(in crate::cli) fn interrupt_key(settings: &Effective, name: &Agent) -> Option<&'static str> {
    agent_integration(settings, name).interrupt_key()
}

/// Trust the path of `workspace`, reopened from `removed`, for each agent
/// whose session `shoal resume` continues there as a tracked agent, since
/// resuming does not trust it as a launch does. Failures only warn.
pub(in crate::cli) async fn trust_resumed(
    ctx: &Context,
    workspace: &Workspace,
    removed: &RemovedSpace,
) {
    let stopped = removed
        .agents
        .iter()
        .filter(|agent| agent.record.session.is_some())
        .map(|agent| agent.record.agent.clone());
    let last = removed
        .last_sessions
        .iter()
        .filter(|last| last.session.is_some())
        .map(|last| last.agent.clone());
    trust_for(ctx, workspace, stopped.chain(last).collect()).await;
}

/// Trust the path of `workspace` for each of `agents` as a tracked launch
/// does. Failures only warn.
async fn trust_for(ctx: &Context, workspace: &Workspace, agents: Vec<Agent>) {
    let settings = match shoal_workers::settings::of(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
    )
    .await
    {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!(
                "warning: cannot trust {} for its agents: {error:#}",
                workspace.path.display()
            );
            return;
        }
    };
    for name in &agents {
        if let Some(trust) = agent_integration(&settings, name).trust_file() {
            let profile = &settings.launch(name).environment;
            trust::tracked(trust, ctx, workspace, profile).await;
        }
    }
}

#[cfg(test)]
mod tests;
