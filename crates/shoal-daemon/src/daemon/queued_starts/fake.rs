//! The queue start worker tests drive: it records each start with the
//! environment its worker would receive.
use anyhow::Result;
use shoal_ids::queue_entry_ids::QueueEntryId;
use std::{collections::BTreeMap, sync::Mutex};

use crate::daemon::agent_queue::Start;

/// Each start with the environment its worker would receive.
pub static STARTED: Mutex<Vec<(QueueEntryId, BTreeMap<String, String>)>> = Mutex::new(Vec::new());

pub(super) fn start(start: &Start) -> Result<()> {
    STARTED
        .lock()
        .unwrap()
        .push((start.id.clone(), start.env.clone()));
    Ok(())
}
