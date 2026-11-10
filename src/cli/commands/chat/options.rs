//! How the command line starts a chat: its agent, place, name, model,
//! session, parent and the permissions given to its agent, as its arguments
//! give them and give them again, checked before any picker opens.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Result, ensure};

use super::{agent::checked_agent, handoff};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::client::{agent::AgentSessionId, space_ids::SpaceId};
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
}

/// Where a chat runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::commands) enum ChatPlace {
    /// In a new focused place beside this terminal, where a host opens one,
    /// otherwise here.
    Beside,
    /// In this terminal instead of a new tab or detached session.
    Here,
    /// In a new background place, for the user to attach to.
    Background,
}

impl ChatPlace {
    /// `--here`, else beside this terminal.
    pub fn here(here: bool) -> Self {
        match here {
            true => Self::Here,
            false => Self::Beside,
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
        }
    }

    /// `start` with these options, as [`Options::taken_from`] takes them
    /// again, except that a background place is written as beside this
    /// terminal, which an agent's start takes for one.
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
        } = self;
        let (permissions, recorded_narrowing) = narrowing.written();
        StartArgs {
            agent,
            here: place == ChatPlace::Here,
            name,
            model: ModelArgs { model, effort },
            args,
            session,
            fork_session: fork,
            restore,
            handoff,
            parent,
            permissions,
            recorded_narrowing,
            ..start
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

    /// These options as an agent's start takes them: its chat goes below
    /// its own space, in a background place rather than beside it or here,
    /// so its processes are not the agent's. Only that place runs it here,
    /// with the scope its handoff carries.
    pub fn for_caller(self, ctx: &Context) -> Result<Self> {
        if let Some(path) = &self.handoff {
            handoff::receive(path)?;
        }
        ensure_scope_token(ctx)?;
        if !crate::env::is_scoped() {
            return Ok(self);
        }
        Ok(match self.place {
            ChatPlace::Beside => Self {
                place: ChatPlace::Background,
                ..self
            },
            ChatPlace::Here => {
                ensure!(
                    handoff::awaited(),
                    "scoped processes start chats in a background place; leave out --here"
                );
                self
            }
            ChatPlace::Background => self,
        })
    }

    /// Refuse a `--name` the daemon would refuse, before any picker opens.
    /// A restored chat keeps its own.
    pub async fn check_name(&self, ctx: &Context) -> Result<()> {
        match (&self.name, &self.restore) {
            (Some(name), None) => crate::client::chats::check_name(&ctx.paths, name).await,
            _ => Ok(()),
        }
    }

    /// Refuse the agent `--agent` or `agent.default` names under
    /// `settings`, or a `--model` it cannot take, before any picker opens.
    pub fn check_agent(&mut self, settings: &Effective) -> Result<()> {
        self.agent = checked_agent(settings, self.agent.take(), &self.model)?;
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
