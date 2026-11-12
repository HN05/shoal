//! Whether a command runs in the user's own terminal or in a process that
//! inherits a space's scope, as the CLI reads it from its environment.
use std::path::Path;

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
