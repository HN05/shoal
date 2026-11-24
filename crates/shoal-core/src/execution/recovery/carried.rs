//! What a restored agent carries over from the saved agent it restores
//! until its hooks report a session of its own: that agent's session and how
//! its space changed since it ran. An agent that exits or stops before
//! reporting one, such as a converted agent whose first start in the new
//! place fails, then leaves the same restore behind instead of one without
//! its session.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    AGENT_SUFFIX, Copied, HandoffBrief, LAST_SESSION_SUFFIX, LastSession, LastSessionExt as _,
    Record, RecordExt as _, consume, reported_session, rewrite,
};
use crate::paths::{Paths, StateFiles as _};

/// The session an execution continues, with how its space changed since
/// the agent that reported it ran.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carried {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    /// As [`Record::moved`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// As [`Record::copied`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied: Option<Copied>,
    /// As [`Record::converted`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub converted: bool,
    /// As [`Record::handoff`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffBrief>,
}

impl Carried {
    /// What the saved agent or last session at `path` carries; nothing for
    /// any other record, such as a stopped command's, or one that is gone.
    fn saved_at(path: &Path) -> Result<Option<Self>> {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if name.ends_with(AGENT_SUFFIX) {
            return Ok(Record::load(path)?.map(|record| Self {
                session: record.session,
                moved: record.moved,
                copied: record.copied,
                converted: record.converted,
                handoff: record.handoff,
            }));
        }
        if name.ends_with(LAST_SESSION_SUFFIX) {
            return Ok(LastSession::load(path)?.map(|last| Self {
                session: last.session,
                moved: last.moved,
                copied: last.copied,
                converted: last.converted,
                handoff: last.handoff,
            }));
        }
        Ok(None)
    }

    /// `record`, which keeps no session of its own, with this session and
    /// these changes of its space.
    pub fn kept_in(self, record: Record) -> Record {
        Record {
            session: self.session,
            moved: record.moved || self.moved,
            copied: record.copied.or(self.copied),
            converted: record.converted || self.converted,
            handoff: record.handoff.or(self.handoff),
            ..record
        }
    }
}

/// Keep, for execution `id` of the space `space_id`, what the saved agent
/// among the records at `restored_from` it restores carries, read before the
/// execution consumes them. A restore of none carries nothing.
pub fn carry(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    restored_from: &[PathBuf],
) -> Result<()> {
    for path in restored_from {
        if let Some(carried) = Carried::saved_at(path)? {
            std::fs::create_dir_all(paths.space_state(space_id))?;
            return rewrite(&carried_path(paths, space_id, id), &carried);
        }
    }
    Ok(())
}

/// The session execution `id` continues: the one its hooks reported, as it
/// is now, else what it carries from the saved agent it restored.
pub fn continued(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Carried {
    if let Some(session) = reported_session(paths, space_id, id) {
        return Carried {
            session: Some(session),
            ..Carried::default()
        };
    }
    load(&carried_path(paths, space_id, id)).unwrap_or_default()
}

/// The sessions the space's executions carry.
pub(super) fn sessions(paths: &Paths, space_id: &SpaceId) -> Vec<AgentSessionId> {
    let Ok(entries) = std::fs::read_dir(paths.space_state(space_id)) else {
        return Vec::new();
    };
    (entries.flatten())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(CARRIED_SUFFIX)
        })
        .filter_map(|entry| load(&entry.path())?.session)
        .collect()
}

/// Forget what an ended execution carried.
pub(super) fn forget(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    consume(&carried_path(paths, space_id, id))
}

fn load(path: &Path) -> Option<Carried> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn carried_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
        .join(format!("{id}{CARRIED_SUFFIX}"))
}

const CARRIED_SUFFIX: &str = ".carried.json";

#[cfg(test)]
mod tests;
