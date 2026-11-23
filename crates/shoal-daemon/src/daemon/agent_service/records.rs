//! The records of other subsystems the agents read and write in their own
//! transactions.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{HashMap, HashSet};

use shoal_agents::daemon::{Records, Removals, SpaceFacts, SpaceStatus, Steps, Waiting};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{demos::Launch, scope_tokens::ScopeToken, space_names::SpaceSelector};

use shoal_links::wait::followed::watches_open_pr;
use shoal_store::WORKSPACE_COLUMNS;

use crate::{
    config::{modes::ModeName, permissions::PermissionsConfig},
    daemon::{
        agent_queue, agents,
        approvals::{self, ApprovalRequest},
        changes, links,
        operations::starts,
        progress, ready_marks,
        spaces::{self, Removing},
        store, threads,
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

    fn space_done(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        store::exists(db, "SELECT 1 FROM space_completion WHERE space_id=?1", [id])
    }

    fn space_is_workspace(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        store::exists(
            db,
            "SELECT 1 FROM spaces WHERE id=?1 AND kind='workspace'",
            [id],
        )
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

    fn space_is_chat(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        links::space::is_chat(db, id)
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

    fn spaces(&self, db: &Connection) -> Result<Vec<SpaceFacts>> {
        Ok(db
            .prepare(&format!("{SPACE_FACTS} ORDER BY s.rowid"))?
            .query_map([], space_facts)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn space_facts(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceFacts>> {
        Ok(db
            .query_row(&format!("{SPACE_FACTS} WHERE s.id=?1"), [id], space_facts)
            .optional()?)
    }

    fn workspace(&self, db: &Connection, id: &SpaceId) -> Result<Option<Workspace>> {
        Ok(db
            .query_row(
                &format!(
                    "SELECT {WORKSPACE_COLUMNS} FROM spaces JOIN workspaces USING(id) WHERE id=?1"
                ),
                [id],
                shoal_store::workspace,
            )
            .optional()?)
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
            .prepare(
                "SELECT space_id FROM space_conversions
                 WHERE json_extract(record,'$.to')='workspace'",
            )?
            .query_map([], |row| row.get(0))?
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
                        .last_post()
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

impl Removals for Removing {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        Removing::require_taking_work(self, db, id)
    }

    fn being_removed(&self, id: &SpaceId) -> bool {
        self.reserved(id)
    }
}

/// What [`space_facts`] reads of a space, before its condition.
const SPACE_FACTS: &str = "SELECT s.id,s.kind,s.repository_id,s.name,w.state,
    COALESCE(w.setup_finished,0),
    EXISTS (SELECT 1 FROM space_completion c WHERE c.space_id=s.id)
    FROM spaces s LEFT JOIN workspaces w USING(id)";

fn space_facts(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceFacts> {
    Ok(SpaceFacts {
        id: row.get(0)?,
        kind: row.get(1)?,
        repository: row.get(2)?,
        name: row.get(3)?,
        state: row.get(4)?,
        setup_finished: row.get(5)?,
        done: row.get(6)?,
    })
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
