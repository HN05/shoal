//! What forge responses taught, kept per account the daemon acts as.
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use crate::forge::transport::memory::Memory;

#[derive(Default)]
pub(crate) struct ForgeMemories(Mutex<HashMap<String, Memory>>);

impl ForgeMemories {
    /// The memory of forge responses to the account `identity` names.
    pub(crate) fn of(&self, identity: &str) -> Memory {
        let mut memories = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memories.entry(identity.to_owned()).or_default().clone()
    }
}
