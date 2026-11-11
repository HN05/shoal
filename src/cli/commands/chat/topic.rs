//! What a chat is about beyond its repository: its default name, mode,
//! subject and links, the workspace it runs in, and how it reopens in a new
//! place.
use std::ffi::OsString;

use super::Options;
use crate::client::{space_ids::SpaceId, space_names::SpaceSelector};
use crate::{
    cli::{agents::ChatRepository, args::create::StartArgs},
    config::templates,
    model::{Chat, ChatHome, LinkedItem, Relation, Repository, SpaceLinks, Workspace},
    repository,
};

/// What a chat is about beyond its repository, and how it reopens in a new tab.
pub(in crate::cli::commands) struct Topic {
    /// The command that starts the same chat again in a new place.
    pub relaunch: Relaunch,
    /// The chat's default name, suffixed `-2`, `-3`, ... while another holds it.
    pub name: String,
    /// The mode the agent starts in.
    pub mode: crate::config::modes::ModeName,
    /// What the agent starts on, or `None` to wait for the user.
    pub subject: Option<templates::Subject>,
    /// The issue the chat is linked to, whose fields the rules of a chat
    /// restored without its subject take.
    pub linked_issue: Option<templates::IssueFields>,
    /// What the chat is linked to once recorded, such as the issue it plans.
    pub links: Vec<(Relation, LinkedItem)>,
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
            name: format!("{}-chat", repository::name(repo)),
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
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
            name: mode.to_string(),
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
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
            name: format!("{}-chat", workspace.name),
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
            workspace: Some(workspace),
            tells_resumed: false,
        }
    }

    /// A chat in `mode` in the checkout of `repo` with `links`, imported
    /// from another machine. One linked to an issue reopens in a new tab
    /// with the issue's fields, for its rules, without asking the forge,
    /// which may have closed it.
    pub(in crate::cli::commands) fn imported(
        repo: &Repository,
        mode: &crate::config::modes::ModeName,
        links: &SpaceLinks,
    ) -> Self {
        let topic = Self::repository(repo, mode);
        let linked = StartArgs {
            linked_issue: links.issue.clone(),
            linked_issue_title: links.issue.as_ref().and(links.issue_title.clone()),
            ..in_checkout(repo)
        };
        Self {
            relaunch: Relaunch::start(mode, linked),
            links: links.relations(),
            linked_issue: templates::IssueFields::linked(links),
            ..topic
        }
    }

    /// This topic for a chat that was a workspace, whose session, which it
    /// continues in `home`, is told so.
    pub(in crate::cli::commands) fn converted(self, home: &std::path::Path) -> Self {
        let told = format!(
            "Shoal converted this workspace into a chat. You now work in the repository's checkout at {}. The workspace's worktree is gone; its branch stays, and shoal restore brings its files back from the checkpoint Shoal kept.",
            home.display()
        );
        Self {
            subject: Some(templates::Subject::told(told)),
            tells_resumed: true,
            ..self
        }
    }

    /// Where the chat about this topic in `repo` runs.
    pub(super) fn home<'a>(&'a self, repo: &'a ChatRepository) -> ChatHome<'a> {
        repo.home(self.workspace.as_ref())
    }
}

/// `topic` for a chat that restores the saved chat `restore` among `chats`
/// in `home`: with the issue that one is linked to when the topic names
/// none, for its restored agent's rules, and told what its space is now when
/// it was a workspace. A restore relaunched in a new place names only the
/// chat, so its topic comes from the saved chat there.
pub(super) fn with_saved(
    topic: Topic,
    chats: &[Chat],
    restore: &SpaceId,
    home: &std::path::Path,
) -> Topic {
    let Some(saved) = chats.iter().find(|chat| chat.id == *restore) else {
        return topic;
    };
    let topic = match topic.linked_issue {
        Some(_) => topic,
        None => Topic {
            linked_issue: templates::IssueFields::linked(&saved.links),
            ..topic
        },
    };
    match saved.converted {
        true => topic.converted(home),
        false => topic,
    }
}

/// The `shoal start` arguments of a chat in the checkout of `repo`.
fn in_checkout(repo: &Repository) -> StartArgs {
    StartArgs {
        repository: Some(repo.id.to_string()),
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
            chat: true,
            mode: Some(mode.clone()),
            ..args
        }))
    }

    /// The same command, starting its chat on the issue at `url`.
    pub(super) fn on_issue(self, url: &str) -> Self {
        match self {
            Self::Start(start) => Self::Start(Box::new(StartArgs {
                issue: Some(Some(url.to_owned())),
                ..*start
            })),
            Self::ConfigSetup => Self::ConfigSetup,
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
