//! Converting a chat into a workspace, which the daemon then prepares in an
//! operation, and a workspace into a chat.
use std::collections::BTreeMap;

use anyhow::Result;

use crate::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    connection::{self, Daemon},
    model::Workspace,
    protocol::{CreationOptions, OperationMethod, WorkspaceMethod, operations::StartedWorkspace},
};

/// The workspace a chat becomes.
pub struct NewWorkspace {
    /// The chat.
    pub chat: SpaceSelector,
    /// The branch the workspace works on, an existing one or a new one.
    pub branch: String,
    /// The issue to link, by number or URL.
    pub issue: Option<String>,
    /// The branch a new branch starts from.
    pub base: Option<String>,
    pub options: CreationOptions,
    /// The mode its agent restarts in; `worker` without one.
    pub mode: Option<ModeName>,
}

/// Turn a chat into the workspace `new` describes and start preparing it
/// with `env`.
pub async fn to_workspace(
    daemon: &impl Daemon,
    new: NewWorkspace,
    env: BTreeMap<String, String>,
) -> Result<StartedWorkspace> {
    let NewWorkspace {
        chat,
        branch,
        issue,
        base,
        options,
        mode,
    } = new;
    let method = OperationMethod::ConvertToWorkspace {
        chat,
        branch,
        issue,
        base,
        options,
        mode,
        env: Some(env),
    };
    connection::request(daemon, method).await
}

/// Make `workspace` a chat in `mode`, `chat` without one, once its agent's
/// turn ends.
pub async fn to_chat(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    mode: Option<ModeName>,
) -> Result<Workspace> {
    connection::request(daemon, WorkspaceMethod::ConvertToChat { workspace, mode }).await
}
