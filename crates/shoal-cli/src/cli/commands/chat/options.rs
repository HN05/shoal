//! How the command line starts a chat: its agent, place, name, model,
//! session, parent and the permissions given to its agent, as its arguments
//! give them and give them again, checked before any picker opens.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use super::handoff;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::client::{agent::AgentSessionId, priority::Priority, space_ids::SpaceId};
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        args::{ModelArgs, create::StartArgs, service::ConfigSetupArgs},
        context::Context,
    },
    config::{
        Effective,
        permissions::{PermissionChange, PermissionsConfig},
    },
};

/// How the command line starts a chat.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::cli::commands) struct Options {
    /// Agent to start; defaults to `agent.default`, otherwise a picker.
    pub agent: Option<Agent>,
    /// Where the chat runs.
    pub place: ChatPlace,
    /// The chat's name; defaults to the topic's.
    pub name: Option<SpaceName>,
    /// The model and effort requested over the configured ones.
    pub model: ModelChoice,
    /// Arguments forwarded to the agent.
    pub args: Vec<OsString>,
    /// The agent session the chat continues, such as one `shoal move
    /// import` brought, instead of starting a new one.
    pub session: Option<AgentSessionId>,
    /// Whether the chat starts from a fork of `session`, which stays
    /// unchanged, instead of continuing it.
    pub fork: bool,
    /// The stopped chat, by ID, that this start restores under its own name
    /// instead of recording a new one.
    pub restore: Option<SpaceId>,
    /// The handoff of a starter that waits on this place to start the chat,
    /// with its scope when it is scoped.
    pub handoff: Option<PathBuf>,
    /// The space the chat goes below; the daemon puts a scoped starter's
    /// below its own space without one.
    pub parent: Option<SpaceSelector>,
    /// The permissions given to the agent, whatever the mode's.
    pub narrowing: Narrowing,
    /// The priority set on the chat.
    pub priority: Option<Priority>,
}

/// Where a chat runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::commands) enum ChatPlace {
    /// In a new place beside this terminal, focused or in the background
    /// as its mode says, where a host opens one, otherwise here.
    Beside,
    /// In this terminal instead of a new tab or detached session.
    Here,
    /// In a new background place this process opens, for the user to
    /// attach to.
    Background,
    /// In a new background place the daemon opens for a new chat, as it
    /// does for an app, for the user to attach to.
    Detached,
}

impl ChatPlace {
    /// `--here`, else beside this terminal.
    pub fn here(here: bool) -> Self {
        match here {
            true => Self::Here,
            false => Self::Beside,
        }
    }

    /// Where a chat that a process, `scoped` or not and `on_terminal` or
    /// not, starts in this place runs: one beside a terminal runs detached
    /// for a scoped process, whose chats are not its own processes, and for
    /// one without a terminal, which nothing could run it beside.
    fn for_starter(self, scoped: bool, on_terminal: bool) -> Self {
        match self {
            Self::Beside if scoped || !on_terminal => Self::Detached,
            place => place,
        }
    }
}

impl Options {
    /// How `args` start a chat, taken from them.
    pub fn taken_from(args: &mut StartArgs) -> Self {
        Self {
            agent: args.agent.take(),
            place: ChatPlace::here(args.here),
            name: args.name.take(),
            model: std::mem::take(&mut args.model).into(),
            args: std::mem::take(&mut args.args),
            session: args.session.take(),
            fork: args.fork_session,
            restore: args.restore.take(),
            handoff: args.handoff.take(),
            parent: args.parent.take(),
            narrowing: match args.recorded_narrowing.take() {
                Some(recorded) => Narrowing::Recorded(recorded),
                None => Narrowing::Requested(PermissionChange::combined(&args.permissions).set),
            },
            priority: args.priority.take(),
        }
    }

    /// `start` with these options, as [`Options::taken_from`] takes them
    /// again, except that a background place is written as beside this
    /// terminal, which an agent's start takes for one, and a fork without a
    /// session as none.
    pub(super) fn into_start(self, start: StartArgs) -> StartArgs {
        let Self {
            agent,
            place,
            name,
            model: ModelChoice { model, effort },
            args,
            session,
            fork,
            restore,
            handoff,
            parent,
            narrowing,
            priority,
        } = self;
        let (permissions, recorded_narrowing) = narrowing.written();
        // A fork without a session, as a copy imported with new sessions
        // asks, starts a new session, as `--fork-session` alone cannot say.
        let fork_session = fork && session.is_some();
        StartArgs {
            agent,
            here: place == ChatPlace::Here,
            name,
            model: ModelArgs { model, effort },
            args,
            session,
            fork_session,
            restore,
            handoff,
            parent,
            permissions,
            recorded_narrowing,
            priority,
            ..start
        }
    }

    /// These options as the start in the place where a chat opens takes
    /// them: run there by `agent` under `name`, reporting to the `handoff`
    /// of the starter that waits on it.
    pub(super) fn in_new_place(
        &self,
        agent: &Agent,
        name: &SpaceName,
        handoff: Option<&Path>,
    ) -> Self {
        Self {
            agent: Some(agent.clone()),
            place: ChatPlace::Here,
            name: Some(name.clone()),
            handoff: handoff.map(Path::to_owned),
            ..self.clone()
        }
    }

    /// `shoal config setup` with these options, which takes neither a
    /// fork, a parent nor permissions: its chat starts on no fork, below no
    /// named space, with its mode's permissions.
    pub(super) fn into_config_setup(self) -> ConfigSetupArgs {
        let Self {
            agent,
            place,
            name,
            model: ModelChoice { model, effort },
            args,
            session,
            fork: _,
            restore,
            handoff,
            parent: _,
            narrowing: _,
            priority: _,
        } = self;
        ConfigSetupArgs {
            here: place == ChatPlace::Here,
            agent,
            model: ModelArgs { model, effort },
            name,
            session,
            restore,
            handoff,
            args,
        }
    }

    /// These options as this process's start takes them: a scoped one's
    /// chat goes below its own space, and one without a terminal or a
    /// scoped one's runs detached rather than beside it. A scoped process
    /// runs a chat here only as the place a handoff carries its scope to.
    pub fn for_caller(self, ctx: &Context) -> Result<Self> {
        ensure_scope_token(ctx)?;
        let scoped = crate::env::is_scoped();
        ensure!(
            !scoped || self.place != ChatPlace::Here || handoff::awaited(),
            "scoped processes start chats in a background place; leave out --here"
        );
        let place = self.place.for_starter(scoped, ctx.on_terminal());
        Ok(Self { place, ..self })
    }

    /// Refuse the agent `--agent` or `agent.default` names under
    /// `settings`, or a `--model` it cannot take, before any picker opens.
    pub fn check_agent(&mut self, settings: &Effective) -> Result<()> {
        self.agent = crate::agents::checked_agent(settings, self.agent.take(), &self.model)?;
        Ok(())
    }

    /// Refuse requested permissions this scoped process does not hold for
    /// an action in the registered repository `repository`, before any
    /// picker opens; recorded ones stay as they are.
    pub async fn check_given(&self, ctx: &Context, repository: Option<&str>) -> Result<()> {
        let Narrowing::Requested(given) = self.narrowing else {
            return Ok(());
        };
        let repository = repository.map(crate::repository::Selector::from);
        crate::cli::commands::permissions::ensure_held(ctx, repository, &given).await
    }
}

/// The permissions given to a chat's agent, by where they came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::cli::commands) enum Narrowing {
    /// The `--permission` its starter asked for, which a scoped starter
    /// must hold.
    Requested(PermissionsConfig),
    /// The permissions a restored or imported chat was given, with what a
    /// resume's `--permission` changed, which the resume checked and the
    /// daemon checks again for what differs from the chat's.
    Recorded(PermissionsConfig),
}

impl Narrowing {
    pub(super) fn value(self) -> PermissionsConfig {
        match self {
            Self::Requested(narrowing) | Self::Recorded(narrowing) => narrowing,
        }
    }

    /// The `--permission` changes and the `--recorded-narrowing` of
    /// `shoal start` that give this narrowing again; an empty recorded one
    /// gives the same permissions as none.
    fn written(self) -> (Vec<PermissionChange>, Option<PermissionsConfig>) {
        match self {
            Self::Requested(narrowing) => (narrowing.changes(), None),
            Self::Recorded(narrowing) if narrowing.is_empty() => (Vec::new(), None),
            Self::Recorded(narrowing) => (Vec::new(), Some(narrowing)),
        }
    }
}

/// Refuse a workspace process that cleared its scope token, which the
/// daemon would take for the user; with the token its chats go below its
/// own space.
pub(in crate::cli::commands) fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start and ask chats only with their scope token"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
