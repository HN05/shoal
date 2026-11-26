//! Where an agent runs, a workspace or a chat in its home, and what its
//! command takes from there: the placeholders its configured command fills
//! in, the place its mode's rules describe, and Shoal's arguments.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::{Context as _, Result};

use super::{agent_integration, args, delivery};
use crate::{
    agent::Agent,
    config::{Effective, named_commands, templates},
    model::{Chat, ChatHome, Repository, SpaceLinks, Workspace},
};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

/// The space an agent runs in.
#[derive(Clone, Copy)]
pub enum AgentHome<'a> {
    Workspace(&'a Workspace),
    /// A chat running in `home`, about the repository named `repo`, or
    /// without one.
    Chat {
        chat: &'a Chat,
        home: ChatHome<'a>,
        repo: Option<&'a str>,
    },
}

impl<'a> From<&'a Workspace> for AgentHome<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self::Workspace(workspace)
    }
}

impl<'a> AgentHome<'a> {
    pub fn id(self) -> &'a SpaceId {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat { chat, .. } => &chat.id,
        }
    }

    pub fn path(self) -> &'a Path {
        match self {
            Self::Workspace(workspace) => &workspace.path,
            Self::Chat { home, .. } => home.path(),
        }
    }

    fn links(self) -> &'a SpaceLinks {
        match self {
            Self::Workspace(workspace) => &workspace.links,
            Self::Chat { chat, .. } => &chat.links,
        }
    }

    /// The place agent `name`'s rules describe, which receives its updates
    /// as its integration and `settings` allow.
    pub fn place(self, settings: &Effective, name: &Agent) -> templates::Place<'a> {
        let delivery = delivery(agent_integration(settings, name), settings);
        match self {
            Self::Workspace(workspace) => templates::Place::Workspace {
                workspace,
                delivery,
            },
            Self::Chat { home, repo, .. } => templates::Place::Chat {
                repo,
                home,
                delivery,
            },
        }
    }

    /// The system text of `mode` for agent `name` here, with the fields of
    /// the issue the space links.
    pub fn system(
        self,
        settings: &Effective,
        name: &Agent,
        mode: &crate::config::modes::ModeName,
    ) -> Result<String> {
        let issue = templates::IssueFields::linked(self.links());
        templates::system(settings, mode, self.place(settings, name), issue.as_ref())
    }

    /// What agent `name` runs with here besides its instructions, prompt and
    /// forwarded arguments: Shoal's hooks, its MCP servers and the other
    /// working directories it may use, `workspaces_dir` among them for a
    /// workspace, as `home`, the user's home, resolves the configured ones.
    pub fn args(
        self,
        settings: &Effective,
        name: &Agent,
        home: &Path,
        workspaces_dir: Option<&Path>,
    ) -> Result<Vec<OsString>> {
        match self {
            Self::Workspace(workspace) => {
                args::workspace_args(settings, name, workspace, home, workspaces_dir)
            }
            Self::Chat { chat, .. } => {
                let mut args = args::space_args(settings, name, super::AgentSpace::Chat)?;
                args.extend(args::chat_dir_args(settings, name, home, chat));
                Ok(args)
            }
        }
    }

    /// `name`'s command among `commands` here, with `fields` beside the
    /// space's own placeholders: a workspace's, with its `diff_base` when the
    /// command uses it, or a chat's `{path}`.
    pub fn expand(
        self,
        commands: &named_commands::Commands,
        name: &str,
        args: Vec<OsString>,
        fields: &[(&str, &OsStr)],
        diff_base: Option<&CommitId>,
    ) -> Result<Vec<OsString>> {
        match self {
            Self::Workspace(workspace) => named_commands::expand_with_fields(
                commands, name, workspace, args, fields, diff_base,
            ),
            Self::Chat { home, .. } => {
                let argv =
                    (commands.get(name)).with_context(|| format!("unknown command {name:?}"))?;
                let mut all = vec![("{path}", home.path().as_os_str())];
                all.extend_from_slice(fields);
                named_commands::substitute_outside_workspace(name, argv, &all, args)
            }
        }
    }
}

/// The records of a space an agent runs in as the daemon has them now,
/// which an [`AgentHome`] borrows: a workspace's, or a chat's with its
/// repository and the workspace whose worktree it runs in.
#[allow(
    clippy::large_enum_variant,
    reason = "read once by the restore that reads it"
)]
pub enum HomeRecords {
    Workspace(Workspace),
    Chat {
        chat: Chat,
        repository: Option<Repository>,
        workspace: Option<Workspace>,
    },
}

impl HomeRecords {
    /// The space as an agent there sees it.
    pub fn home(&self) -> Result<AgentHome<'_>> {
        Ok(match self {
            Self::Workspace(workspace) => AgentHome::Workspace(workspace),
            Self::Chat {
                chat,
                repository,
                workspace,
            } => {
                let home = match (workspace, repository, chat.workdir.root_dir()) {
                    (Some(workspace), ..) => ChatHome::Workspace(workspace),
                    (None, Some(repository), _) => ChatHome::Checkout(repository),
                    (None, None, Some(root)) => ChatHome::Root(root),
                    (None, None, None) => anyhow::bail!("chat {} has no home", chat.name),
                };
                AgentHome::Chat {
                    chat,
                    home,
                    repo: repository.as_ref().map(Repository::display_name),
                }
            }
        })
    }
}
