//! Launching the agents Shoal runs, and handing a workspace to a desktop
//! app.
pub mod desktop;
mod detached;
mod environment;
mod sessions;
mod tracked;
mod trust;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use crate::{
    agent::{Agent, ModelChoice},
    agents::{AgentHome, HomeRecords, agent_integration, ensure_forks, resolve_model},
    client::connection::CommandPaths,
    config::{Effective, modes, templates},
    hosting::Caller,
    model::{RemovedSpace, Workspace},
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

pub use desktop::{ensure_opens, hand_off};
pub use sessions::{SessionHome, install_kept_session_files, place_forked_sessions};

/// One launch of an agent in a space, a ready workspace or a chat; settings
/// are never cached across launches.
pub struct Launch {
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
pub enum Opening {
    /// A new session, starting on `subject` when it has one, with `note`
    /// after its mode's instructions.
    New {
        mode: modes::ModeName,
        subject: Option<templates::Subject>,
        note: Option<String>,
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
            Self::New {
                mode,
                subject,
                note,
            } => {
                let place = home.place(settings, name);
                let brief = templates::brief(settings, mode, place, subject.as_ref())?;
                Ok(noted(brief, note.as_deref()))
            }
            Self::Told { mode, message } => Ok(templates::Brief {
                system: home.system(settings, name, mode)?,
                first_message: Some(message.clone()),
            }),
        }
    }
}

/// `brief` with `note` ending its first message.
fn noted(brief: templates::Brief, note: Option<&str>) -> templates::Brief {
    let Some(note) = note else {
        return brief;
    };
    let first_message = match brief.first_message {
        Some(first) => format!("{first}\n\n{note}"),
        None => note.to_owned(),
    };
    templates::Brief {
        first_message: Some(first_message),
        ..brief
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
pub struct LaunchOptions {
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
pub enum ModelRequest {
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
pub fn runs_in_session(agent: &Agent, settings: &Effective) -> bool {
    !runs_its_own_way(agent, settings) && crate::terminal::keeps_agents_in_sessions(settings)
}

/// Whether a launch of `agent` under `settings` leaves the launching process
/// at once, as one its integration runs detached, such as Happy, does.
pub fn runs_its_own_way(agent: &Agent, settings: &Effective) -> bool {
    agent_integration(settings, agent).detached().is_some()
}

/// Start an agent in `space`, a ready workspace or a chat as its records
/// say, giving it the prompt the way it accepts one.
pub async fn launch_agent(
    caller: Caller<'_>,
    agent: Agent,
    space: HomeRecords,
    opening: Opening,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let home = space.home()?;
    let target = ConfigTarget::Space(SpaceSelector::id(home.id()));
    let settings = crate::settings::of(caller.paths, target).await?;
    let integration = agent_integration(&settings, &agent);
    let fork = match &options.fork {
        Some(session) => {
            sessions::find_forked_session(caller.paths, &space, (&settings, &agent), session).await;
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
        return tracked::tracked(caller, integration, launch).await;
    };
    let code = detached::start(caller, integration, detached, launch).await?;
    // A launch of its own returns once its agent has started.
    for record in &replaces {
        shoal_core::execution::recovery::consume(record)?;
    }
    Ok(code)
}

/// The arguments that start agent `name` in `directory` from a fork of
/// `session`, refusing an agent [`ensure_forks`] refuses.
pub fn fork_args(
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
pub fn interrupt_key(settings: &Effective, name: &Agent) -> Option<&'static str> {
    agent_integration(settings, name).interrupt_key()
}

/// Trust the path of `workspace`, reopened from `removed`, for each agent
/// whose session `shoal resume` continues there as a tracked agent, since
/// resuming does not trust it as a launch does. Failures only warn.
pub async fn trust_resumed(paths: &CommandPaths, workspace: &Workspace, removed: &RemovedSpace) {
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
    trust_for(paths, workspace, stopped.chain(last).collect()).await;
}

/// Trust the path of `workspace` for each of `agents` as a tracked launch
/// does. Failures only warn.
async fn trust_for(paths: &CommandPaths, workspace: &Workspace, agents: Vec<Agent>) {
    let settings =
        match crate::settings::of(paths, ConfigTarget::Space(SpaceSelector::id(&workspace.id)))
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
            let home = AgentHome::Workspace(workspace);
            let added = crate::homes::dirs(paths, home, &settings, name).await;
            trust::tracked(trust, paths, workspace, profile, &added).await;
        }
    }
}

#[cfg(test)]
mod tests;
