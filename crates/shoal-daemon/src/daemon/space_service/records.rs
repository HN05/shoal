//! The records of the daemon's other subsystems that the spaces read and write
//! in their own transactions.
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{Connection, Transaction};
use shoal_forge_types::items::link::ItemKind;
use shoal_forge_types::items::reference::ItemRef;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::agent::Agent;
use shoal_protocol::executions::TurnRetry;
use shoal_protocol::model::{Chat, LinkedItem, Relation, Repository, Space, SpaceLinks, Workspace};
use shoal_protocol::state::AgentState;
use shoal_spaces::daemon::Dropped;

use crate::daemon::{
    coordination_service::CoordinationRecords, executions, idle_wakes, messages, restarts,
    turn_retry, turn_state, wake_timers,
};

/// The records of the daemon's other subsystems, as the spaces read and
/// write them in their own transactions.
pub(crate) struct SpaceRecords {
    pub(crate) coordination: CoordinationRecords,
}

impl shoal_spaces::daemon::Records for SpaceRecords {
    fn load_workspace(&self, db: &Connection, workspace: &mut Workspace) -> Result<()> {
        load_space(db, workspace)?;
        workspace.latest_review = shoal_reviews::store::latest_status(
            db,
            &crate::daemon::review_service::ReviewRecords,
            &workspace.id,
        )?;
        Ok(())
    }

    fn load_chat(&self, db: &Connection, chat: &mut Chat) -> Result<()> {
        load_space(db, chat)
    }

    fn space_links(&self, db: &Connection, id: &SpaceId) -> Result<SpaceLinks> {
        Ok(shoal_links::linked::space_links(db, id)?)
    }

    fn assigned_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        kind: Option<ItemKind>,
    ) -> Result<Vec<ItemRef>> {
        shoal_links::linked::assigned_items(db, id, kind)
    }

    fn recorded_items(
        &self,
        db: &Connection,
        id: &SpaceId,
        relation: Relation,
    ) -> Result<Vec<LinkedItem>> {
        Ok(shoal_links::linked::recorded_items(db, id, relation)?)
    }

    fn agent_state(&self, db: &Connection, id: &SpaceId) -> Result<Option<AgentState>> {
        Ok(turn_state::current(db, id)?.map(|status| status.state))
    }

    fn hand_alerts_to_user(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<i64>> {
        messages::hand_to_user(tx, id)
    }

    fn remove_issue_links(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
    ) -> Result<(Vec<Dropped>, Vec<ItemRef>)> {
        let assignees = (crate::daemon::links::issue::removed_assignees(tx, id)?.into_iter())
            .map(|dropped| Dropped {
                url: dropped.url,
                login: dropped.login,
            })
            .collect();
        Ok((
            assignees,
            crate::daemon::links::issue::removed_issues(tx, id)?,
        ))
    }

    fn drop_pending_reviews(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        shoal_reviews::written::drop_pending_of_removed(
            tx,
            &crate::daemon::review_service::ReviewRecords,
            id,
        )
    }

    fn cancel_wake(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        turn_retry::cancel(tx, id)?;
        wake_timers::cancel(tx, id)
    }

    fn turn_retry(&self, db: &Connection, id: &SpaceId) -> Result<Option<TurnRetry>> {
        turn_retry::get(db, id)
    }

    fn other_executions(
        &self,
        db: &Connection,
        space: Option<&SpaceId>,
        own: Option<&ExecutionId>,
    ) -> Result<Vec<(SpaceId, ExecutionId)>> {
        executions::other_executions(db, space, own)
    }

    fn running_agents(&self, db: &Connection, id: &SpaceId) -> Result<Vec<ExecutionId>> {
        executions::running_agents(db, id)
    }

    fn queued_agent(&self, db: &Connection, id: &SpaceId) -> Result<Option<Agent>> {
        shoal_agents::queue::store::queued_agent(db, id)
    }

    fn launch_waits_to_open(&self, db: &Connection, id: &SpaceId) -> Result<bool> {
        shoal_agents::queue::store::launch_waits_to_open(db, id)
    }

    fn forget_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        executions::forget_space(tx, id)
    }

    fn forget_command_executions(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        executions::forget_commands(tx, id)
    }

    fn interrupt_operations(&self, tx: &Transaction<'_>) -> Result<()> {
        shoal_leases::sim::interrupt_clean_requests(tx)?;
        executions::interrupt_running(tx)
    }

    fn repositories(&self, db: &Connection) -> Result<Vec<Repository>> {
        shoal_repositories::records::all(db)
    }

    fn touch_repository(&self, tx: &Transaction<'_>, id: &RepositoryId) -> Result<()> {
        shoal_repositories::records::touch(tx, id)
    }

    fn leasing_workspaces(&self, db: &Connection, name: &str) -> Result<Vec<SpaceId>> {
        shoal_leases::resources::holding_workspaces(db, name)
    }

    fn next_space_number(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
        crate::daemon::hub_service::numbering::next(tx, id)
    }

    fn ensure_holders_fit(&self, tx: &Transaction<'_>, paths: &[PathBuf]) -> Result<()> {
        messages::ensure_holders_fit(tx, paths)
    }

    fn coordination(&self) -> &dyn shoal_coordination::daemon::Records {
        &self.coordination
    }
}

/// Fills in what the other subsystems keep of every space: its agent's
/// state, sleep, wake, restart and place in the queue, and its links.
fn load_space<P>(db: &Connection, space: &mut Space<P>) -> Result<()> {
    let id = &space.id;
    space.agent_state = turn_state::current(db, id)?;
    space.sleep = idle_wakes::sleep::current(db, id)?;
    space.wake_at = wake_timers::due_at(db, id)?;
    space.asleep = crate::daemon::agent_queue::asleep::current(db, id)?;
    space.restart = restarts::record::current(db, id)?;
    space.links = shoal_links::linked::space_links(db, id)?;
    Ok(())
}
