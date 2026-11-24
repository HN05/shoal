//! How the daemon reaches the forges: the accounts its requests go out as,
//! each remembering what the forge's responses taught it, with the agent
//! account's logins for Git, which forge each host serves, and what the
//! forge says of the items that block work.
mod accounts;
mod blockers;
mod git_logins;
mod hosts;
mod memories;

use std::sync::Arc;

pub use hosts::{Change, ForgeHosts, learn_forge_pins};

use crate::{config_state::ConfigState, gates::Gates, store::Store};

pub struct ForgeAccess {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    memories: memories::ForgeMemories,
}

impl ForgeAccess {
    pub fn new(store: Store, gates: Arc<Gates>, config: Arc<ConfigState>) -> Self {
        Self {
            store,
            gates,
            config,
            memories: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests;
