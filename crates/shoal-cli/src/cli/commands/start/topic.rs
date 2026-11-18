//! What a chat is about beyond its repository: its default name, mode,
//! subject and links, the workspace it runs in, and how it reopens in a new
//! place.
use std::ffi::OsString;

use super::Options;
use crate::{
    cli::{agents::ChatRepository, args::create::StartArgs},
    config::templates,
    forge::items::reference::ItemRef,
    model::{ChatHome, Repository, SpaceLinks, Workspace},
};
use shoal_protocol::launches::StartTarget;
use shoal_protocol::space_names::SpaceSelector;

/// What a chat is about beyond its repository, and how it reopens in a new tab.
pub(in crate::cli::commands) struct Topic {
    /// The command that starts the same chat again in a new place.
    pub relaunch: Relaunch,
    /// The chat's default name, which `--name` replaces; without one, it
    /// takes its home's.
    pub name: Option<String>,
    /// The mode the agent starts in.
    pub mode: crate::config::modes::ModeName,
    /// What the agent starts on, or `None` to wait for the user.
    pub subject: Option<templates::Subject>,
    /// The issue the chat is linked to, whose fields the rules of a chat
    /// restored without its subject take.
    pub linked_issue: Option<templates::IssueFields>,
    /// What the chat is linked to once recorded, such as the issue it plans.
    pub links: SpaceLinks,
    /// The workspace whose worktree the chat runs in, instead of the
    /// repository's checkout.
    pub workspace: Option<Workspace>,
    /// Whether a session the chat continues is told the subject too, as
    /// news, such as what its converted space is now.
    pub tells_resumed: bool,
}

impl Topic {
    /// A chat in `mode` in the checkout of `repo`.
    pub(in crate::cli::commands) fn repository(
        repo: &Repository,
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        Self {
            relaunch: Relaunch::start(mode, in_checkout(repo)),
            name: None,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: SpaceLinks::default(),
            workspace: None,
            tells_resumed: false,
        }
    }

    /// A chat in `mode` without a repository, in `root_dir`.
    pub(in crate::cli::commands) fn outside_repositories(
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        Self {
            relaunch: Relaunch::start(mode, StartArgs::default()),
            name: None,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: SpaceLinks::default(),
            workspace: None,
            tells_resumed: false,
        }
    }

    /// A chat in `mode` in the worktree of `workspace`.
    pub(in crate::cli::commands) fn workspace(
        workspace: Workspace,
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        let place = StartArgs {
            in_workspace: Some(Some(SpaceSelector::id(&workspace.id))),
            ..Default::default()
        };
        Self {
            relaunch: Relaunch::start(mode, place),
            name: None,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: SpaceLinks::default(),
            workspace: Some(workspace),
            tells_resumed: false,
        }
    }

    /// This topic for a chat whose space changed since its agent stopped,
    /// told `subject` even as it continues its session.
    pub(in crate::cli::commands) fn changed(self, subject: templates::Subject) -> Self {
        Self {
            subject: Some(subject),
            tells_resumed: true,
            ..self
        }
    }

    /// This topic started on `prompt`, the agent's first message, when it
    /// starts on nothing else, as a workspace's agent starts on its own.
    pub(in crate::cli::commands) fn told(self, prompt: Option<String>) -> Self {
        let subject = (self.subject).or_else(|| prompt.map(templates::Subject::told));
        Self { subject, ..self }
    }

    /// Where the chat about this topic in `repo` runs.
    pub(super) fn home<'a>(&'a self, repo: &'a ChatRepository) -> ChatHome<'a> {
        repo.home(self.workspace.as_ref())
    }
}

/// The `shoal start` arguments of a chat in the checkout of `repo`.
fn in_checkout(repo: &Repository) -> StartArgs {
    StartArgs {
        repository: Some(StartTarget::Repository(repo.id.to_string())),
        ..Default::default()
    }
}

/// The command that starts a chat about a topic again in a new place, with
/// the options its start was given.
#[derive(Debug, Clone)]
pub(in crate::cli::commands) enum Relaunch {
    /// `shoal start` with these arguments.
    Start(Box<StartArgs>),
    /// `shoal config setup`.
    ConfigSetup,
}

impl Relaunch {
    /// `shoal start` of a chat in `mode` with `args`.
    fn start(mode: &crate::config::modes::ModeName, args: StartArgs) -> Self {
        Self::Start(Box::new(StartArgs {
            as_chat: true,
            mode: Some(mode.clone()),
            ..args
        }))
    }

    /// The same command, starting its chat on `issue`.
    pub(super) fn on_issue(self, issue: &ItemRef) -> Self {
        match self {
            Self::Start(start) => Self::Start(Box::new(StartArgs {
                issue: Some(Some(issue.clone().into())),
                ..*start
            })),
            Self::ConfigSetup => Self::ConfigSetup,
        }
    }

    /// The issue the command starts its chat on, if it names one.
    pub(super) fn issue(&self) -> Option<crate::forge::items::reference::ItemInput> {
        match self {
            Self::Start(start) => start.issue.clone().flatten(),
            Self::ConfigSetup => None,
        }
    }

    /// The `shoal` arguments of the command, started with `options`.
    pub(super) fn command_line(&self, options: Options) -> Vec<OsString> {
        match self {
            Self::Start(start) => options.into_start((**start).clone()).command_line(),
            Self::ConfigSetup => options.into_config_setup().command_line(),
        }
    }
}

#[cfg(test)]
mod tests;
