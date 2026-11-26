//! What a space saved for `shoal resume`: its stopped agents, last sessions and
//! interrupted commands.
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{
    AGENT_SUFFIX, COMMAND_SUFFIX, LAST_SESSION_SUFFIX, LastSession, LastSessionExt as _, Record,
    RecordExt as _, Saved, SavedCommand, SavedSession, saved_at,
};
use crate::{
    fsutil::atomic::{Permissions, ReplaceOptions},
    paths::Paths,
    process::identity,
};

/// Whether a stop saved an agent of the space, as a recovery record or a
/// last session. It reads only the file names, so a record that does not
/// parse still counts.
pub fn saves_agent(paths: &Paths, space_id: &SpaceId) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(AGENT_SUFFIX) || name.ends_with(LAST_SESSION_SUFFIX) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A command `shoal stop` interrupted, reported by `shoal resume` instead of
/// being rerun: replaying an arbitrary command is not known to be safe.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CommandRecord {
    pub(crate) argv: Vec<String>,
}

pub fn save_command(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    argv: &[OsString],
) -> Result<PathBuf> {
    // A lossy record would report a different command than the one stopped.
    let record = CommandRecord {
        argv: argv
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or_else(|| anyhow::anyhow!("an argument is not valid UTF-8"))?,
    };
    let directory = paths.space_state(space_id);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{id}{COMMAND_SUFFIX}"));
    crate::fsutil::atomic::replace_atomically(
        &path,
        &serde_json::to_vec(&record)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(path)
}

/// Reading what a workspace saved, which the daemon answers requests with.
pub trait SavedExt: Sized {
    fn load(paths: &Paths, space_id: &SpaceId) -> Result<Self>;
}

impl SavedExt for Saved {
    fn load(paths: &Paths, space_id: &SpaceId) -> Result<Self> {
        let mut saved = Self::default();
        let entries = match std::fs::read_dir(paths.space_state(space_id)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(saved),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(id) = name.strip_suffix(AGENT_SUFFIX) {
                let id = ExecutionId::from(id.to_owned());
                saved.agents.push(stopped_agent(id, path));
            } else if let Some(id) = name.strip_suffix(LAST_SESSION_SUFFIX) {
                let id = ExecutionId::from(id.to_owned());
                saved.sessions.push(last_session(id, path));
            } else if let Some(id) = name.strip_suffix(COMMAND_SUFFIX) {
                let record: CommandRecord = serde_json::from_slice(&std::fs::read(&path)?)
                    .with_context(|| format!("read {}", path.display()))?;
                saved.commands.push(SavedCommand {
                    id: ExecutionId::from(id.to_owned()),
                    path,
                    argv: record.argv,
                });
            }
        }
        saved.agents.sort_by(|a, b| a.id.cmp(&b.id));
        saved.sessions.sort_by(|a, b| a.id.cmp(&b.id));
        saved.commands.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(saved)
    }
}

/// The stopped agent `id` whose record is at `path`.
fn stopped_agent(id: ExecutionId, path: PathBuf) -> SavedSession {
    let record = Record::load(&path).ok().flatten();
    SavedSession {
        stopped_at: saved_at(&path),
        agent: record.as_ref().map(|record| record.agent.clone()),
        stop_reason: record
            .as_ref()
            .and_then(|record| record.stop_reason.clone()),
        idle: record.as_ref().is_some_and(Record::between_turns),
        waits_in_wrapper: record.as_ref().is_some_and(waits),
        origin: record.and_then(|record| record.origin),
        id,
        path,
    }
}

/// The last session `id` saved at `path`, whose agent exited, so it
/// stopped neither idle nor for a reason Shoal knows.
fn last_session(id: ExecutionId, path: PathBuf) -> SavedSession {
    let last = LastSession::load(&path).ok().flatten();
    SavedSession {
        stopped_at: saved_at(&path),
        agent: last.as_ref().map(|last| last.agent.clone()),
        stop_reason: None,
        idle: false,
        waits_in_wrapper: false,
        origin: last.and_then(|last| last.origin),
        id,
        path,
    }
}

/// Whether a live wrapper waits to restore the agent of `record`.
pub(super) fn waits(record: &Record) -> bool {
    (record.waiting.as_ref()).is_some_and(|wrapper| identity::alive(wrapper).unwrap_or(false))
}

/// Whether any workspace has stopped work that waits for `shoal resume`;
/// readable while the daemon is down.
pub fn any_pending(paths: &Paths) -> bool {
    let Ok(entries) = std::fs::read_dir(paths.state.join("workspaces")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_str().is_some_and(|id| {
            let saved = Saved::load(paths, &SpaceId::from(id.to_owned()));
            saved.is_ok_and(|saved| saved.is_restorable())
        })
    })
}

/// Pending recovery is unfinished work even when its Git tree is clean.
pub fn pending(paths: &Paths, space_id: &SpaceId) -> Result<bool> {
    pending_except(paths, space_id, |_| false)
}

/// Whether recovery is pending for executions of the space other than
/// those `excepted` selects.
pub fn pending_except(
    paths: &Paths,
    space_id: &SpaceId,
    excepted: impl Fn(&ExecutionId) -> bool,
) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let execution = name
            .strip_suffix(AGENT_SUFFIX)
            .or_else(|| name.strip_suffix(COMMAND_SUFFIX));
        if execution.is_some_and(|execution| !excepted(&ExecutionId::from(execution.to_owned()))) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The executions of stopped agents that wait for `shoal resume` in the space.
pub fn agent_records(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ExecutionId>> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(id) = name.to_string_lossy().strip_suffix(AGENT_SUFFIX) {
            records.push(ExecutionId::from(id.to_owned()));
        }
    }
    Ok(records)
}
