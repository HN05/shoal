//! Converting a chat into a workspace, which the daemon then prepares in an
//! operation, and a workspace into a chat: the requests of
//! [`ConversionMethod`].
use std::collections::BTreeMap;

use crate::connection::Prepared;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    config::modes::ModeName,
    model::Workspace,
    protocol::{ConversionMethod, CreationOptions, operations::StartedWorkspace},
};

/// The workspace a chat becomes.
pub struct NewWorkspace {
    /// The chat.
    pub chat: SpaceSelector,
    /// The branch the workspace works on, an existing one or a new one.
    pub branch: String,
    /// The issue to link, by number or URL.
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    /// The branch a new branch starts from.
    pub base: Option<String>,
    pub options: CreationOptions,
    /// The mode its agent restarts in; `worker` without one.
    pub mode: Option<ModeName>,
}

/// Turn a chat into the workspace `new` describes and start preparing it
/// with `env`.
pub fn to_workspace(
    new: NewWorkspace,
    env: BTreeMap<String, String>,
) -> Prepared<StartedWorkspace> {
    let NewWorkspace {
        chat,
        branch,
        issue,
        base,
        options,
        mode,
    } = new;
    let method = ConversionMethod::ConvertToWorkspace {
        chat,
        branch,
        issue,
        base,
        options,
        mode,
        env: Some(env),
    };
    Prepared::new(method)
}

/// Make `workspace` a chat in `mode`, `chat` without one, once its agent's
/// turn ends.
pub fn to_chat(workspace: SpaceSelector, mode: Option<ModeName>) -> Prepared<Workspace> {
    Prepared::new(ConversionMethod::ConvertToChat { workspace, mode })
}
