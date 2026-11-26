//! What lets Shoal restore a stopped agent: its recovery record, last
//! sessions and restore requests. A restore command omits the original task
//! and outlives the wrapper.
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::fsutil::atomic::{Permissions, ReplaceOptions};

pub mod carried;
pub mod changes;
pub(crate) mod claim;
mod ending;
mod last_session;
mod permissions;
mod record;
mod restore_requests;
mod saved;
mod sessions;
mod stopped;

pub use claim::{RecoveryClaim, claim, claim_last_session, claim_record, forget_saved};
use ending::idle;
pub(crate) use ending::saved_at;
pub use ending::{Ending, StopExt, resumed_prompt};
pub use last_session::LastSessionExt;
pub use permissions::{ClaimedChange, change_permission};
use record::write_record;
pub use record::{
    RecordExt, Recovery, Restorer, discard_undelivered, keep_as_last_session, record_path,
    save_load_record, save_record, save_stopped, save_turn, saved_path,
};
pub use restore_requests::{forget_restore_request, request_restore, restore_requests};
pub use saved::{
    SavedExt, agent_records, any_pending, pending, pending_except, save_command, saves_agent,
};
use sessions::reported_session;
pub use sessions::{forget_session, held_sessions, save_session, valid_session};
pub use shoal_protocol::{
    executions::{Copied, HandoffBrief, LastSession, Record, Stage, Stop, StoppedAgent},
    saved::{Saved, SavedCommand, SavedSession},
};
pub use stopped::{clear_load_mark, save_stopped_agents, stopped_agents};

const AGENT_SUFFIX: &str = ".recovery.json";
const REQUEST_SUFFIX: &str = ".restore-request.json";
const COMMAND_SUFFIX: &str = ".command.json";
const SESSION_SUFFIX: &str = ".session";
const LAST_SESSION_SUFFIX: &str = ".last-session.json";

fn rewrite(path: &Path, saved: &impl Serialize) -> Result<()> {
    crate::fsutil::atomic::replace_atomically(
        path,
        &serde_json::to_vec(saved)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

pub fn consume(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
