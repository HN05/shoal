//! The handlers of the hooks Shoal installs for the agents it launches,
//! which `shoal internal` runs: each reads the event the agent passes on
//! stdin, reports it for the agent's space, and prints what the agent reads
//! as context.
pub mod input;
pub mod turns;

use anyhow::{Result, bail, ensure};

use crate::{
    client::connection::CommandPaths,
    fsutil::current_dir::current_directory,
    scoping::Scoping,
    workspace_context::{ScopeOrder, WorkspaceContext},
};
use shoal_protocol::space_names::SpaceSelector;

/// The space a hook reports for: the one `explicit` names, else the
/// agent's own chat, else its own workspace.
async fn own_space(paths: &CommandPaths, explicit: Option<SpaceSelector>) -> Result<SpaceSelector> {
    if let Some(space) = explicit {
        return Ok(space);
    }
    if let Some(chat) = crate::env::own_chat() {
        return Ok(SpaceSelector::Id(chat));
    }
    own_workspace(paths).await
}

/// The workspace the agent's scope belongs to, else, unscoped, the one the
/// hook runs in. Hooks report for the agent's own workspace wherever its
/// shell has moved, such as into a child workspace.
async fn own_workspace(paths: &CommandPaths) -> Result<SpaceSelector> {
    let workspaces = crate::client::spaces::list(paths).await?.into_workspaces();
    let (scoping, order) = (Scoping::of_workspace(), ScopeOrder::BeforeDirectory);
    let cwd = match order.reads_directory(scoping) {
        true => current_directory()?,
        false => None,
    };
    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
    if let Some(workspace) = context.resolve(None, scoping, order) {
        return Ok(SpaceSelector::id(&workspace.id));
    }
    ensure!(
        scoping == Scoping::Unscoped,
        "scoped workspace is unavailable"
    );
    bail!("no current workspace; pass an explicit workspace")
}
