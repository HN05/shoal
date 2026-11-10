//! What a chat is about beyond its repository: its default name, mode,
//! subject and links, the workspace it runs in, and how it reopens in a new
//! place.
use std::ffi::OsString;

use crate::client::space_ids::SpaceId;
use crate::{
    cli::agents::ChatRepository,
    config::templates,
    model::{Chat, ChatHome, LinkedItem, Relation, Repository, SpaceLinks, Workspace},
    repository,
};

/// What a chat is about beyond its repository, and how it reopens in a new tab.
pub(in crate::cli::commands) struct Topic {
    /// The `shoal` command that starts the same chat again, without the
    /// agent, name, `--here` or forwarded arguments.
    pub relaunch: Vec<OsString>,
    /// The chat's default name, suffixed `-2`, `-3`, ... while another holds it.
    pub name: String,
    /// Whether an interactive start without `--name` asks for the name.
    pub asks_name: bool,
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
}

impl Topic {
    /// A chat in `mode` in the checkout of `repo`.
    pub(in crate::cli::commands) fn repository(
        repo: &Repository,
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        Self {
            relaunch: [start_args(mode), vec![repo.id.clone().into()]].concat(),
            name: format!("{}-chat", repository::name(repo)),
            asks_name: true,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
            workspace: None,
        }
    }

    /// A chat in `mode` without a repository, in `root_dir`.
    pub(in crate::cli::commands) fn outside_repositories(
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        Self {
            relaunch: start_args(mode),
            name: mode.to_string(),
            asks_name: true,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
            workspace: None,
        }
    }

    /// A chat in `mode` in the worktree of `workspace`.
    pub(in crate::cli::commands) fn workspace(
        workspace: Workspace,
        mode: &crate::config::modes::ModeName,
    ) -> Self {
        let place = vec!["--in".into(), workspace.id.clone().into()];
        Self {
            relaunch: [start_args(mode), place].concat(),
            name: format!("{}-chat", workspace.name),
            asks_name: true,
            mode: mode.clone(),
            subject: None,
            linked_issue: None,
            links: Vec::new(),
            workspace: Some(workspace),
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
        let mut topic = Self::repository(repo, mode);
        if let Some(url) = &links.issue {
            topic.relaunch.extend(["--linked-issue".into(), url.into()]);
        }
        if let Some(title) = links.issue.as_ref().and(links.issue_title.as_ref()) {
            topic
                .relaunch
                .extend(["--linked-issue-title".into(), title.into()]);
        }
        Self {
            links: links.relations(),
            linked_issue: templates::IssueFields::linked(links),
            ..topic
        }
    }

    /// Where the chat about this topic in `repo` runs.
    pub(super) fn home<'a>(&'a self, repo: &'a ChatRepository) -> ChatHome<'a> {
        repo.home(self.workspace.as_ref())
    }
}

/// `topic` with the issue the saved chat `restore` among `chats` is linked
/// to, for its restored agent's rules; a restore relaunched in a new tab
/// names only the chat.
pub(super) fn with_saved_issue(topic: Topic, chats: &[Chat], restore: &SpaceId) -> Topic {
    let saved = chats.iter().find(|chat| chat.id == *restore);
    Topic {
        linked_issue: saved.and_then(|chat| templates::IssueFields::linked(&chat.links)),
        ..topic
    }
}

/// The `shoal` arguments that start a chat in `mode`.
fn start_args(mode: &crate::config::modes::ModeName) -> Vec<OsString> {
    ["start", "--chat", "--mode", mode.as_str()]
        .map(OsString::from)
        .to_vec()
}

#[cfg(test)]
mod tests;
