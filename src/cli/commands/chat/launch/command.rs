//! What a chat's agent runs with: the session it continues or forks, what
//! it is told, and its launcher's command line.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::{Result, ensure};

use super::super::{Options, Topic};
use crate::client::{agent::AgentSessionId, space_names::SpaceName};
use crate::{
    agent::Agent,
    cli::{
        agents::{self, ChatLaunch, ChatRepository},
        commands::configured,
    },
    config::{Effective, templates},
    repository,
};

/// What the chat about `topic` in `repo`, named `name`, passes on to
/// `agent` under `settings`, as `options` ask.
pub(super) fn chat_launch(
    settings: Effective,
    agent: &Agent,
    repo: ChatRepository,
    topic: &Topic,
    options: &Options,
    name: SpaceName,
) -> Result<ChatLaunch> {
    let session = SessionArgs {
        session: options.session.as_ref(),
        fork: options.fork,
        directory: topic.home(&repo).path(),
    };
    let resumed = session.of(&settings, agent)?;
    // An agent that cannot continue or fork the session starts a new one,
    // which has not seen the chat's rules.
    let brief = brief(&settings, agent, &repo, topic, !resumed.is_empty())?;
    Ok(ChatLaunch {
        name,
        mode: topic.mode.clone(),
        narrowing: options.narrowing.value(),
        parent: options.parent.clone(),
        instructions: brief.system,
        opening: brief.first_message,
        model: crate::agents::resolve_model(&settings, agent, options.model.clone()),
        repository: repo,
        workspace: topic.workspace.clone(),
        settings,
        args: [resumed, options.args.clone()].concat(),
        restores: options.restore.clone(),
        links: topic.links.clone(),
    })
}

/// The session a chat's agent continues, or starts in `directory` from a
/// fork of when `fork`.
struct SessionArgs<'a> {
    session: Option<&'a AgentSessionId>,
    fork: bool,
    directory: &'a Path,
}

impl SessionArgs<'_> {
    /// The arguments with which `agent` continues the session or starts from
    /// a fork of it, unless the chat starts a new one; an agent without
    /// such arguments starts a new one, with a warning.
    fn of(&self, settings: &Effective, agent: &Agent) -> Result<Vec<OsString>> {
        let Some(session) = self.session else {
            return Ok(Vec::new());
        };
        ensure!(
            crate::execution::recovery::valid_session(session.as_str()),
            "invalid agent session ID"
        );
        if !self.fork {
            return Ok(resumed_args(settings, agent, session));
        }
        if let Err(error) = crate::agents::ensure_forks(settings, agent) {
            eprintln!("warning: {error:#}, so the chat starts a new session");
            return Ok(Vec::new());
        }
        agents::fork_args(settings, agent, session, self.directory)
    }
}

/// The arguments with which `agent` continues `session`; an agent without
/// resume arguments starts a new one, with a warning.
fn resumed_args(settings: &Effective, agent: &Agent, session: &AgentSessionId) -> Vec<OsString> {
    let resumed = settings.launch(agent).arguments.resume_args(Some(session));
    if resumed.is_none() {
        eprintln!(
            "warning: {} has no resume arguments, so the chat starts a new session",
            agent.name()
        );
    }
    resumed.unwrap_or_default()
}

/// The agent's configured launcher with the chat instructions, its opening
/// task, the hooks that deliver its messages and the arguments that give it
/// `directories`, run in `path`, the chat's checkout or worktree.
pub(super) fn command(
    settings: &Effective,
    directories: Vec<OsString>,
    path: &Path,
    agent: &Agent,
    instructions: String,
    opening: Option<String>,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let name = agent;
    // Instructions that join the prompt start a turn before the user speaks.
    let (instructions, opening) = match settings.launch(name).arguments.instructions {
        Some(_) => (instructions, opening),
        None => (
            templates::chat_first_message(&instructions, opening.as_deref()).unwrap_or_default(),
            None,
        ),
    };
    let (args, prompt) = agents::input(
        settings,
        name,
        instructions,
        opening,
        [
            crate::agents::hook_args(
                settings.integration(name),
                crate::agents::AgentSpace::Chat,
                false,
            )?,
            directories,
        ]
        .concat(),
        args,
    );
    configured::expand_outside_workspace(
        &settings.launchers,
        name.name(),
        &[
            ("{path}", path.as_os_str()),
            ("{prompt}", OsStr::new(&prompt)),
        ],
        args,
    )
}

/// What the chat about `topic` in `repo` is told: its mode's rules, or
/// nothing for a chat that `resumes` a session with an agent that takes
/// them only in its first message, which the session already has; and its
/// subject, unless it resumes a session.
fn brief(
    settings: &Effective,
    agent: &Agent,
    repo: &ChatRepository,
    topic: &Topic,
    resumes: bool,
) -> Result<templates::Brief> {
    let place = templates::Place::Chat {
        repo: repo.registered().map(repository::name),
        home: topic.home(repo),
        delivery: agents::chat_delivery(settings, agent),
    };
    let mut brief = match topic.subject.as_ref().filter(|_| !resumes) {
        Some(subject) => templates::brief(settings, &topic.mode, place, Some(subject))?,
        None => {
            let issue = (topic.subject.as_ref())
                .and_then(|subject| subject.issue.as_ref())
                .or(topic.linked_issue.as_ref());
            templates::Brief {
                system: templates::system(settings, &topic.mode, place, issue)?,
                first_message: None,
            }
        }
    };
    let in_first_message = settings.launch(agent).arguments.instructions.is_none();
    if resumes && in_first_message {
        brief.system.clear();
    }
    Ok(brief)
}

#[cfg(test)]
mod tests;
