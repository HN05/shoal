//! The `shoal restore` tests drive: it records each workspace whose last
//! session a relocation continued.
use shoal_ids::space_ids::SpaceId;
use std::sync::Mutex;

/// The workspaces whose last session a relocation continued.
pub(crate) static RESUMED: Mutex<Vec<SpaceId>> = Mutex::new(Vec::new());

pub(super) fn restore(id: &SpaceId) {
    RESUMED.lock().unwrap().push(id.clone());
}
