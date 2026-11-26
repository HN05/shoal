//! The session each running agent's hooks report, and every session a space
//! holds.
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};
use std::path::PathBuf;

use anyhow::Result;

use super::{LastSession, LastSessionExt as _, SESSION_SUFFIX, carried, consume, stopped_agents};
use crate::{
    fsutil::atomic::{Permissions, ReplaceOptions},
    paths::Paths,
};

/// Whether `session` can name an agent session on a restore command line:
/// letters, digits, `-` and `_`, never read as an option.
pub fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && !session.starts_with('-')
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn session_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
        .join(format!("{id}{SESSION_SUFFIX}"))
}

/// Record the session the hooks of execution `id` report, writing only a
/// change; whether it changed.
pub fn save_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    session: &AgentSessionId,
) -> Result<bool> {
    if reported_session(paths, space_id, id).as_ref() == Some(session) {
        return Ok(false);
    }
    std::fs::create_dir_all(paths.space_state(space_id))?;
    crate::fsutil::atomic::replace_atomically(
        &session_path(paths, space_id, id),
        session.as_str().as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )?;
    Ok(true)
}

pub(super) fn reported_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
) -> Option<AgentSessionId> {
    std::fs::read_to_string(session_path(paths, space_id, id))
        .ok()
        .filter(|session| valid_session(session))
        .map(AgentSessionId::from)
}

/// The agent sessions the space keeps: those its running agents reported
/// or, until they report one, carry from the agents they restored, and
/// those of its stopped agents and last sessions. A record that cannot be
/// read holds none.
pub fn held_sessions(paths: &Paths, space_id: &SpaceId) -> Vec<AgentSessionId> {
    let mut held = Vec::new();
    if let Ok(entries) = std::fs::read_dir(paths.space_state(space_id)) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(id) = name.to_string_lossy().strip_suffix(SESSION_SUFFIX) {
                let id = ExecutionId::from(id.to_owned());
                held.extend(reported_session(paths, space_id, &id));
            }
        }
    }
    let stopped = stopped_agents(paths, space_id).unwrap_or_default();
    held.extend(stopped.into_iter().filter_map(|agent| agent.record.session));
    held.extend(carried::sessions(paths, space_id));
    let last = LastSession::all(paths, space_id).unwrap_or_default();
    held.extend(last.into_iter().filter_map(|last| last.session));
    held
}

/// Forget the session of an execution that has ended, and what it carried.
pub fn forget_session(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    consume(&session_path(paths, space_id, id))?;
    carried::forget(paths, space_id, id)
}
