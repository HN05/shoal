//! The stopped agents a removal record keeps, and the mark of an agent stopped
//! for load.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context as _, Result};

use super::{
    Record, RecordExt as _, Saved, SavedExt as _, SavedSession, StoppedAgent, claim, record_path,
    rewrite, saved_at, write_record,
};
use crate::paths::Paths;

/// Clear the [`Record::for_load`] mark of the agent stopped as `id`, whose
/// restore healthy readings admitted already, so its retry waits only for a
/// free slot, under the claim a restore or permission change takes, which
/// refuses a record one of them holds. A record without one is left as it
/// is.
pub fn clear_load_mark(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    let path = record_path(paths, space_id, id);
    if !Record::load(&path)?.is_some_and(|record| record.for_load) {
        return Ok(());
    }
    let _claim = claim(&path)?;
    match Record::load(&path)? {
        Some(record) => rewrite(
            &path,
            &Record {
                for_load: false,
                ..record
            },
        ),
        None => Ok(()),
    }
}

/// The workspace's stopped agents, for its removal record. Their wrappers have
/// exited, so none waits to restore them, and the restored workspace waits
/// for `shoal resume` to restore them.
pub fn stopped_agents(paths: &Paths, space_id: &SpaceId) -> Result<Vec<StoppedAgent>> {
    let mut agents = Vec::new();
    for SavedSession {
        id: execution,
        path,
        ..
    } in Saved::load(paths, space_id)?.agents
    {
        if let Some(mut record) = Record::load(&path)? {
            record.waiting = None;
            let stopped_at = saved_at(&path).context("read when the agent stopped")?;
            agents.push(StoppedAgent {
                execution,
                record,
                stopped_at,
            });
        }
    }
    Ok(agents)
}

/// Save a removed workspace's stopped agents for the workspace restoring it.
pub fn save_stopped_agents(
    paths: &Paths,
    space_id: &SpaceId,
    agents: &[StoppedAgent],
) -> Result<()> {
    for agent in agents {
        let path = write_record(paths, space_id, &agent.execution, agent.record.clone())?;
        std::fs::File::open(path)?.set_modified(agent.stopped_at.system_time())?;
    }
    Ok(())
}
