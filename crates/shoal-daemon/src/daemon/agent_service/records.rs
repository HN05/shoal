//! The records of other subsystems the agents read and write in their own
//! transactions.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{HashMap, HashSet};

use shoal_agents::daemon::{Records, Removals, SpaceStatus, Steps, Waiting};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    demos::Launch, scope_tokens::ScopeToken, space_names::SpaceSelector, state::ConversionTarget,
};

use shoal_links::wait::followed::watches_open_pr;
use shoal_store::WORKSPACE_COLUMNS;

use crate::{
    config::{modes::ModeName, permissions::PermissionsConfig},
    daemon::{
        agent_queue, agents,
        approvals::{self, ApprovalRequest},
        changes, links, progress, ready_marks,
        spaces::{self, Removing},
        starts, store, threads,
        tree::{self, TrackedAgents},
    },
    model::{PreparationWait, SpaceKind, Workspace},
    state::WorkspaceState,
};

pub(crate) struct AgentRecords;

impl Records for AgentRecords {
    fn end_commands(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        changes::records::end_commands(tx, id)
    }

    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool> {
        agents::stop_recorded(db, id)
    }

    fn wake_asleep(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        agent_queue::asleep::wake(db, id)
    }

    fn receives_open_pr_activity(
        &self,
        db: &Connection,
        id: &SpaceId,
        running: &[SpaceId],
    ) -> Result<bool> {
        let agents = TrackedAgents::of(running);
        let children = tree::delegated(db, id, &agents)?
            .into_iter()
            .map(|child| child.id);
        for space in std::iter::once(id.to_owned()).chain(children) {
            if watches_open_pr(db, &space)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ensure_takes_alerts(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        let present: Option<bool> = db
            .query_row(
                "SELECT state IS NOT ?2 FROM spaces LEFT JOIN workspaces USING(id) WHERE id=?1",
                params![id, WorkspaceState::Removing],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(present.is_some(), "no space {id}");
        ensure!(present == Some(true), "the workspace is being removed");
        Ok(())
    }

    fn selected_space(
        &self,
        db: &Connection,
        selector: &SpaceSelector,
    ) -> Result<Option<(SpaceKind, SpaceId)>> {
        spaces::selected(db, selector)
    }

    fn parent_ids(&self, db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
        tree::parent_ids(db)
    }

    fn agent_token(&self, db: &Connection, id: &SpaceId) -> Result<Option<ScopeToken>> {
        shoal_coordination::scope::tokens::agent_token(db, id)
    }

    fn repository_registered(&self, db: &Connection, id: &RepositoryId) -> Result<bool> {
        store::exists(db, "SELECT 1 FROM repositories WHERE id=?1", [id])
    }

    fn preparation_waits(&self, db: &Connection, id: &SpaceId) -> Result<Option<PreparationWait>> {
        starts::preparation_waits(db, id)
    }

    fn note_preparation_stopped(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        starts::note_stopped(db, id)
    }

    fn agent_ended(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        links::issue::agent_ended(tx, id)
    }

    fn release_decided_by(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
    ) -> Result<Vec<ApprovalRequest>> {
        approvals::release_decided_by(tx, id)
    }

    fn record_demo(
        &self,
        tx: &Transaction<'_>,
        id: &ExecutionId,
        space: &SpaceId,
        launch: &Launch,
    ) -> Result<()> {
        shoal_demos::records::record(tx, id, space, launch)
    }

    fn mark_demo_run(&self, tx: &Transaction<'_>, id: &ExecutionId) -> Result<()> {
        shoal_demos::records::mark_run(tx, id)
    }

    fn demo_launch(&self, db: &Connection, id: &ExecutionId) -> Result<Option<Launch>> {
        shoal_demos::records::execution_launch(db, id)
    }

    fn issue_workspaces(
        &self,
        db: &Connection,
        repository: &RepositoryId,
        url: &str,
    ) -> Result<Vec<Workspace>> {
        Ok(db
            .prepare(&format!(
                "SELECT {WORKSPACE_COLUMNS} FROM spaces JOIN workspaces USING(id)
                 WHERE repository_id=?1
                 AND id IN (SELECT space_id FROM space_issue WHERE url=?2) ORDER BY name"
            ))?
            .query_map(params![repository, url], shoal_store::workspace)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn converting_to_workspaces(&self, db: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(db
            .prepare("SELECT space_id FROM space_conversions WHERE target=?1")?
            .query_map([ConversionTarget::Workspace], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn recorded_job_workspaces(&self, db: &Connection) -> Result<HashSet<SpaceId>> {
        Ok(db
            .prepare("SELECT workspace_id FROM ci_jobs")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn runners_registered(&self, db: &Connection) -> Result<bool> {
        store::exists(db, "SELECT 1 FROM runners", [])
    }

    fn record_space_mode(
        &self,
        db: &Connection,
        id: &SpaceId,
        mode: &ModeName,
        narrowing: &PermissionsConfig,
    ) -> Result<()> {
        db.execute(
            "UPDATE spaces SET mode=?2,narrowing=?3 WHERE id=?1",
            params![id, mode, shoal_store::narrowing_column(narrowing)?],
        )?;
        Ok(())
    }

    fn reserve_setup(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
        let reserved = tx.execute(
            "UPDATE workspaces SET state=?2,error=NULL,setup_finished=0,setup_skipped=0
             WHERE id=?1 AND state IN (?3,?4,?2)",
            params![
                id,
                WorkspaceState::Preparing,
                WorkspaceState::Ready,
                WorkspaceState::Failed
            ],
        )?;
        Ok(reserved == 1)
    }

    fn end_setup(&self, tx: &Transaction<'_>, id: &SpaceId, error: Option<&str>) -> Result<()> {
        let state = match error {
            None => WorkspaceState::Ready,
            Some(_) => WorkspaceState::Failed,
        };
        tx.execute(
            "UPDATE workspaces SET state=?2,error=?3,setup_finished=?4 WHERE id=?1 AND state=?5",
            params![id, state, error, error.is_none(), WorkspaceState::Preparing],
        )?;
        Ok(())
    }

    fn space_status(
        &self,
        db: &Connection,
        id: &SpaceId,
        pushing: &dyn Fn(&str) -> bool,
    ) -> Result<Option<SpaceStatus>> {
        let Some(name) = space_name(db, id)? else {
            return Ok(None);
        };
        Ok(Some(SpaceStatus {
            name,
            thread: threads::newest_waiting_for_user(db, id)?.map(|thread| Waiting {
                number: thread.number,
                about: first_line(
                    thread
                        .last_turn()
                        .map_or(&thread.name, |post| &post.post.text),
                ),
            }),
            approval: approvals::oldest_for_user(db, id, pushing)?.map(|request| Waiting {
                number: request.number,
                about: request.subject(),
            }),
            done: spaces::done::recorded(db, id)?.map(|completion| completion.summary),
            ready: (ready_marks::list(db, id)?.into_iter().next())
                .map(|mark| mark.url.map(|url| url.url().to_owned())),
            progress: progress::current(db, id)?.map_or_else(Steps::default, |progress| Steps {
                done: progress.done(),
                total: progress.steps.len(),
                line: progress.status,
            }),
        }))
    }
}

/// The removals under way, as agents ask them whether a space takes work.
pub(crate) struct SpaceRemovals(pub(crate) Removing);

impl Removals for SpaceRemovals {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        Removing::require_taking_work(&self.0, db, id)
    }

    fn being_removed(&self, id: &SpaceId) -> bool {
        self.0.reserved(id)
    }
}

fn space_name(db: &Connection, id: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}
