//! What an agent's command takes from its space as the daemon records it:
//! the space's records, the directory where its repository keeps its
//! workspaces, which the agent may also work in, and the diff base its
//! configured command names.
use shoal_protocol::model::ByKind;
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use anyhow::{Context as _, Result};
use shoal_refs::CommitId;

use crate::{
    agent::Agent,
    agents::{AgentHome, HomeRecords},
    client::connection::{CommandPaths, Daemon},
    config::{
        Effective,
        named_commands::{Commands, uses_diff_base},
    },
    model::Workspace,
};
use shoal_protocol::space_names::SpaceSelector;

/// The records of the space `selector` names.
pub async fn records(paths: &CommandPaths, selector: &SpaceSelector) -> Result<HomeRecords> {
    let spaces = crate::client::spaces::list(paths).await?;
    match spaces
        .find(selector)
        .with_context(|| format!("no space {selector}"))?
        .by_kind()
    {
        ByKind::Workspace(workspace) => Ok(HomeRecords::Workspace(workspace)),
        ByKind::Chat(chat) => {
            let workspace = chat
                .workdir
                .workspace()
                .and_then(|workspace| spaces.find(&SpaceSelector::id(&workspace.id)))
                .and_then(|space| space.to_workspace());
            let repository = match &chat.repository_id {
                Some(id) => Some(
                    (crate::client::repositories::list(paths).await?.into_iter())
                        .find(|repository| repository.id == *id)
                        .with_context(|| format!("no repository {id}"))?,
                ),
                None => None,
            };
            Ok(HomeRecords::Chat {
                chat,
                repository,
                workspace,
            })
        }
    }
}

/// What agent `name` runs with in `home` besides its instructions, prompt
/// and forwarded arguments, as [`AgentHome::args`] gives it.
pub async fn args(
    paths: &CommandPaths,
    home: AgentHome<'_>,
    settings: &Effective,
    name: &Agent,
) -> Result<Vec<OsString>> {
    let workspaces_dir = match home {
        AgentHome::Workspace(workspace) => workspaces_dir(paths, workspace).await,
        AgentHome::Chat { .. } => None,
    };
    home.args(settings, name, &paths.home, workspaces_dir.as_deref())
}

/// The other working directories agent `name` may use in `home`, as
/// [`crate::agents::workspace_dir_args`] gives them a workspace's agent and
/// [`crate::agents::chat_dir_args`] a chat's.
pub async fn dir_args(
    paths: &CommandPaths,
    home: AgentHome<'_>,
    settings: &Effective,
    name: &Agent,
) -> Vec<OsString> {
    match home {
        AgentHome::Workspace(workspace) => {
            let workspaces_dir = workspaces_dir(paths, workspace).await;
            let workspaces_dir = workspaces_dir.as_deref();
            crate::agents::workspace_dir_args(settings, name, &paths.home, workspaces_dir)
        }
        AgentHome::Chat { chat, .. } => {
            crate::agents::chat_dir_args(settings, name, &paths.home, chat)
        }
    }
}

/// `name`'s command among `commands` in `home`, with `fields` beside the
/// space's own placeholders, as [`AgentHome::expand`] fills it in.
pub async fn expand(
    daemon: &impl Daemon,
    home: AgentHome<'_>,
    commands: &Commands,
    name: &str,
    args: Vec<OsString>,
    fields: &[(&str, &OsStr)],
) -> Result<Vec<OsString>> {
    let diff_base = match home {
        AgentHome::Workspace(workspace) => diff_base(daemon, workspace, commands.get(name)).await?,
        AgentHome::Chat { .. } => None,
    };
    home.expand(commands, name, args, fields, diff_base.as_ref())
}

/// The directory in which `workspace`'s repository keeps its workspaces,
/// resolved as agents compare paths; `None`, with a warning, when it cannot be
/// looked up.
async fn workspaces_dir(paths: &CommandPaths, workspace: &Workspace) -> Option<PathBuf> {
    let directory = crate::client::repositories::list(paths)
        .await
        .and_then(|repositories| {
            repositories
                .into_iter()
                .find(|repository| repository.id == *workspace.repository())
                .map(|repository| repository.workspaces_dir)
                .context("workspace repository is missing")
        });
    match directory {
        Ok(directory) => Some(std::fs::canonicalize(&directory).unwrap_or(directory)),
        Err(error) => {
            eprintln!("warning: could not give the agent the workspaces directory: {error:#}");
            None
        }
    }
}

/// The workspace's diff base commit when `argv`, its command, uses it.
async fn diff_base(
    daemon: &impl Daemon,
    workspace: &Workspace,
    argv: Option<&Vec<String>>,
) -> Result<Option<CommitId>> {
    if !argv.is_some_and(|argv| uses_diff_base(argv)) {
        return Ok(None);
    }
    let base = crate::client::diffs::diff_base(daemon, SpaceSelector::id(&workspace.id)).await?;
    Ok(Some(base.commit))
}
