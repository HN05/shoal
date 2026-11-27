//! The records the stand-in keeps of the other subsystems' tables.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    approvals::ApprovalRequest,
    config::{modes::ModeName, permissions::PermissionsConfig},
    demos::Launch,
    model::{PreparationWait, SpaceKind, Workspace},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};

use crate::daemon::{Records, Removals, SpaceStatus, Steps};

/// No removals: the stand-in removes no space.
pub(crate) struct StandInRemovals;

impl Removals for StandInRemovals {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        shoal_store::require_ready(db, id)
    }

    fn being_removed(&self, _: &SpaceId) -> bool {
        false
    }
}

/// Records without the other subsystems' tables: no commands an agent
/// left, no restores Shoal queued, the spaces a test makes watch an open
/// PR, the workspaces it records a CI job running in and the scope tokens
/// of the chats it records.
#[derive(Default)]
pub(crate) struct StandInRecords {
    pub(crate) open_prs: Mutex<HashSet<SpaceId>>,
    pub(crate) job_workspaces: Mutex<HashSet<SpaceId>>,
    pub(super) chat_tokens: Mutex<HashMap<SpaceId, ScopeToken>>,
}

impl Records for StandInRecords {
    fn end_commands(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_stops WHERE execution_id=?1)",
            [id],
            |row| row.get(0),
        )?)
    }

    fn wake_asleep(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn receives_open_pr_activity(
        &self,
        _: &Connection,
        id: &SpaceId,
        _: &[SpaceId],
    ) -> Result<bool> {
        Ok(self.open_prs.lock().unwrap().contains(id))
    }

    fn ensure_takes_alerts(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        let present = shoal_store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [id])?;
        anyhow::ensure!(present, "no space {id}");
        Ok(())
    }

    fn repository_registered(&self, db: &Connection, id: &RepositoryId) -> Result<bool> {
        shoal_store::exists(db, "SELECT 1 FROM repositories WHERE id=?1", [id])
    }

    fn selected_space(
        &self,
        db: &Connection,
        selector: &SpaceSelector,
    ) -> Result<Option<(SpaceKind, SpaceId)>> {
        let Some(id) = selector.given_id() else {
            return Ok(None);
        };
        Ok(db
            .query_row("SELECT kind FROM spaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?
            .map(|kind| (kind, id.to_owned())))
    }

    fn parent_ids(&self, db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
        Ok(db
            .prepare("SELECT space_id, parent_id FROM space_parents")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn agent_token(&self, _: &Connection, id: &SpaceId) -> Result<Option<ScopeToken>> {
        Ok(self.chat_tokens.lock().unwrap().get(id).cloned())
    }

    fn preparation_waits(&self, _: &Connection, _: &SpaceId) -> Result<Option<PreparationWait>> {
        Ok(None)
    }

    fn note_preparation_stopped(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn agent_ended(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn release_decided_by(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    fn record_demo(
        &self,
        _: &Transaction<'_>,
        _: &ExecutionId,
        _: &SpaceId,
        _: &Launch,
    ) -> Result<()> {
        Ok(())
    }

    fn mark_demo_run(&self, _: &Transaction<'_>, _: &ExecutionId) -> Result<()> {
        Ok(())
    }

    fn demo_launch(&self, _: &Connection, _: &ExecutionId) -> Result<Option<Launch>> {
        Ok(None)
    }

    fn issue_workspaces(
        &self,
        _: &Connection,
        _: &RepositoryId,
        _: &str,
    ) -> Result<Vec<Workspace>> {
        Ok(Vec::new())
    }

    fn converting_to_workspaces(&self, _: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(HashSet::new())
    }

    fn recorded_job_workspaces(&self, _: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(self.job_workspaces.lock().unwrap().clone())
    }

    fn runners_registered(&self, _: &Connection) -> Result<bool> {
        Ok(false)
    }

    fn record_space_mode(
        &self,
        _: &Connection,
        _: &SpaceId,
        _: &ModeName,
        _: &PermissionsConfig,
    ) -> Result<()> {
        Ok(())
    }

    fn reserve_setup(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<bool> {
        Ok(true)
    }

    fn end_setup(&self, _: &Transaction<'_>, _: &SpaceId, _: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn space_status(
        &self,
        db: &Connection,
        id: &SpaceId,
        _: &dyn Fn(&str) -> bool,
    ) -> Result<Option<SpaceStatus>> {
        let name: Option<String> = db
            .query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(name.map(|name| SpaceStatus {
            name,
            thread: None,
            approval: None,
            done: None,
            ready: None,
            progress: Steps::default(),
        }))
    }
}
