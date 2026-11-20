//! The issues, PRs and CI runs the daemon's spaces link. [`Links`] holds
//! what links keep in memory and offers their operations.
pub mod issue;
pub mod linked;
mod memories;
pub mod pr;
pub mod wait;

use shoal_forge::transport::memory::Memory;

/// The links' service, which `Manager` holds.
#[derive(Default)]
pub struct Links {
    memories: memories::ForgeMemories,
    waits: wait::ActiveWaits,
}

impl Links {
    /// The running wait of each space.
    pub fn waits(&self) -> &wait::ActiveWaits {
        &self.waits
    }

    /// The memory of forge responses to the account `identity` names.
    pub fn forge_memory(&self, identity: &str) -> Memory {
        self.memories.of(identity)
    }
}
