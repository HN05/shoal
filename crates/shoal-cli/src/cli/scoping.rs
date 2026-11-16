//! Whether a command runs in the user's own terminal or in a process that
//! inherits a space's scope, as the CLI reads it from its environment.
use std::path::Path;

use crate::model::{Space, Spaces};

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

    /// Scoped when this process carries a scope that names its own
    /// workspace; a chat's scope names none.
    pub fn of_workspace() -> Self {
        Self::when(crate::env::workspace_scope())
    }

    /// The spaces among `spaces`, those this process sees, that it acts on
    /// together: every one unscoped, and scoped those its scope reaches but
    /// its own, since a chat also sees the workspace it runs in, which it
    /// only reads.
    pub fn reached_below(self, spaces: Spaces) -> Vec<Space> {
        if self == Self::Unscoped {
            return spaces.0;
        }
        let own = crate::env::own_space();
        let home = (spaces.iter())
            .find(|space| own.as_ref() == Some(&space.id))
            .and_then(|space| space.workdir.workspace())
            .map(|workspace| workspace.id.clone());
        (spaces.0.into_iter())
            .filter(|space| Some(&space.id) != own.as_ref() && Some(&space.id) != home.as_ref())
            .collect()
    }

    fn when(scoped: bool) -> Self {
        match scoped {
            true => Self::Scoped,
            false => Self::Unscoped,
        }
    }
}
