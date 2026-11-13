//! Whether a command runs in the user's own terminal or in a process that
//! inherits a space's scope, as the CLI reads it from its environment.
use std::path::Path;

use crate::model::{PresentSpace, Spaces};

/// Whether a command runs unscoped, in the user's own terminal, or scoped,
/// in a process that inherits a space's scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scoping {
    Unscoped,
    Scoped,
}

impl Scoping {
    /// Scoped when this process or an ancestor carries scope for the `state`
    /// directory, so a cleared scope token does not unscope it.
    pub fn inherited(state: &Path) -> Self {
        Self::when(crate::env::inherits_scope(state))
    }

    /// Scoped when this process carries a workspace's scope, which names its
    /// own workspace; a chat's scope names none.
    pub fn of_workspace() -> Self {
        Self::when(crate::env::workspace_scoped())
    }

    fn when(scoped: bool) -> Self {
        match scoped {
            true => Self::Scoped,
            false => Self::Unscoped,
        }
    }
}

/// The spaces among `spaces`, those this process sees, that its scope
/// reaches but its own: a chat also sees the workspace it runs in, which it
/// only reads.
pub fn reached_below(spaces: Spaces) -> Vec<PresentSpace> {
    let own = crate::env::own_space();
    let home = (spaces.chats.iter())
        .find(|chat| own.as_ref() == Some(&chat.id))
        .and_then(|chat| chat.workspace.as_ref())
        .map(|workspace| workspace.id.clone());
    let workspaces = spaces.workspaces.into_iter().map(PresentSpace::Workspace);
    let chats = spaces.chats.into_iter().map(PresentSpace::Chat);
    (workspaces.chain(chats))
        .filter(|space| Some(space.id()) != own.as_ref() && Some(space.id()) != home.as_ref())
        .collect()
}
