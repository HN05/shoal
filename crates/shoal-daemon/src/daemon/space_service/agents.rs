//! The agents the spaces start, stop and restore, through the agents' service.
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge_types::items::reference::ItemInput;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::agent::Agent;
use shoal_protocol::executions::ProxyRetry;
use shoal_protocol::model::{
    Chat, Execution, QueueAdmission, QueueEntry, RunningAgent, Space, StoppedSpace,
};
use shoal_protocol::processes::Identity;
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::QueuedLaunches;
use shoal_protocol::protocol::operations::{LaunchRequest, QueuedLaunch};
use shoal_protocol::sessions::Origin;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::state::BlockerKind;
use shoal_spaces::daemon::CallerAgent;
use tokio::sync::MutexGuard;

use crate::daemon::{alerts::AlertKind, manager::Manager, scope::Caller};
use crate::repository::Selector;
use shoal_agents::messages::Delivery;
use shoal_daemon_base::blockers::Readiness;
use shoal_daemon_base::stops::{StopPolicy, StopRequest};

impl shoal_spaces::daemon::Agents for Manager {
    fn agents_suspended(&self) -> bool {
        self.agents.agent_queue().suspended()
    }

    fn opening_readiness<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Readiness>>> {
        Box::pin(self.agents.opening_readiness(self, id))
    }

    fn cleanup_orphan_sessions(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.agents.cleanup_orphan_sessions(self))
    }

    fn queue_start_agent<'a>(
        &'a self,
        place: (&'a Space, bool),
        launch: QueuedLaunch,
        follows: (Option<ItemInput>, Vec<Blocker>),
    ) -> BoxFuture<'a, Result<QueueAdmission>> {
        Box::pin(self.agents.queue_start_agent(self, place, launch, follows))
    }

    fn queue_attach<'a>(
        &'a self,
        id: &'a QueueEntryId,
        space: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.queue_attach(self, id, space))
    }

    fn disk_holds_back_starts(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(self.agents.disk_holds_back_starts(self))
    }

    fn lock_agent_admissions(&self) -> BoxFuture<'_, MutexGuard<'_, ()>> {
        Box::pin(self.agents.agent_admissions().lock())
    }

    fn lock_connections(&self) -> BoxFuture<'_, Box<dyn Send + '_>> {
        Box::pin(async move {
            let connections: Box<dyn Send + '_> =
                Box::new(self.agents.executions().connections().await);
            connections
        })
    }

    fn execution_connected<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool> {
        Box::pin(self.agents.executions().is_connected(id))
    }

    fn forget_workspace_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.executions().forget_workspace(id))
    }

    fn agent_turn<'a>(
        &'a self,
        space: &'a Space,
        execution: Option<&'a ExecutionId>,
    ) -> BoxFuture<'a, Result<(ExecutionId, Agent)>> {
        Box::pin(self.agents.agent_turn(space, execution))
    }

    fn agent_environments<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<BTreeMap<String, String>>>> {
        Box::pin(self.agents.agent_environments(id))
    }

    fn agent_wrappers(&self) -> BoxFuture<'_, Result<Vec<Identity>>> {
        Box::pin(self.agents.agent_wrappers())
    }

    fn annotate_stops<'a>(&'a self, executions: &'a mut [Execution]) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.annotate_stops(executions))
    }

    fn caller_agent<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Option<CallerAgent>> {
        Box::pin(async move {
            let agent = self.agents.caller_agent(caller).await?;
            Some(CallerAgent {
                execution: agent.execution,
                name: agent.name,
                space_id: agent.space_id,
            })
        })
    }

    fn cancel_stopped_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.cancel_stopped_restores(self, workspace_id))
    }

    fn chat_environment<'a>(
        &'a self,
        chat: Chat,
    ) -> BoxFuture<'a, Result<BTreeMap<String, OsString>>> {
        Box::pin(async move {
            let home = self.agents.chat_execution_home(self, chat).await?;
            let (_, values, _) = self.agents.joining_environment(home).await?;
            Ok(values)
        })
    }

    fn check_blockers<'a>(
        &'a self,
        repository: &'a Selector,
        blockers: Vec<Blocker>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.check_blockers(self, repository, blockers))
    }

    fn cleared_locally<'a>(
        &'a self,
        repository: Option<&'a RepositoryId>,
        kind: BlockerKind,
        target: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.agents.cleared_locally(self, repository, kind, target))
    }

    fn close_space_places<'a>(&'a self, id: &'a SpaceId, name: &'a SpaceName) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.close_space_places(self, id, name))
    }

    fn ensure_may_replace<'a>(
        &'a self,
        caller: &'a Caller,
        selector: &'a mut SpaceSelector,
        replaces: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.agents
                .ensure_may_replace(self, caller, selector, replaces),
        )
    }

    fn ensure_replaceable<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.ensure_replaceable(space))
    }

    fn discard_replaced_agent<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.discard_replaced_worker(self, space))
    }

    fn ensure_takes_worker<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.ensure_takes_worker(space))
    }

    fn forget_execution<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.forget_execution(self, id))
    }

    fn forget_last_sessions(&self, workspace_id: &SpaceId) {
        self.agents.forget_last_sessions(workspace_id)
    }

    fn forget_reports(&self, id: &SpaceId) {
        self.agents.forget_reports(id)
    }

    fn has_queued_launch<'a>(&'a self, space_id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.agents.has_queued_launch(space_id))
    }

    fn idle_restores<'a>(
        &'a self,
        workspace_id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<HashSet<ExecutionId>>> {
        Box::pin(self.agents.idle_restores(self, workspace_id))
    }

    fn note_interaction<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.note_interaction(self, selector))
    }

    fn record_user_interaction<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.record_user_interaction(self, caller, selector))
    }

    fn owned_places(
        &self,
        space: Option<SpaceId>,
    ) -> BoxFuture<'_, Result<Vec<(SpaceId, Origin)>>> {
        Box::pin(self.agents.owned_places(space))
    }

    fn proxy_retry(&self, executions: &[Execution]) -> Option<ProxyRetry> {
        self.agents.proxy_retry(executions)
    }

    fn queue_agent<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        launch: LaunchRequest,
        blockers: Vec<Blocker>,
        pr: Option<ItemInput>,
    ) -> BoxFuture<'a, Result<QueueEntry>> {
        Box::pin(
            self.agents
                .queue_agent(self, workspace, launch, blockers, pr),
        )
    }

    fn queue_agent_message_next_turn<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.agents
                .queue_agent_message_delivered(id, kind, message, Delivery::NextTurn),
        )
    }

    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>> {
        Box::pin(self.agents.running_agents())
    }

    fn saved_executions<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Vec<ExecutionId>>> {
        Box::pin(async move {
            let saved = self.agents.saved_work(self, selector).await?;
            Ok(saved.agent_ids().cloned().collect())
        })
    }

    fn start_stopped_restore<'a>(
        &'a self,
        workspace: &'a SpaceSelector,
        execution: &'a ExecutionId,
        env: BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<QueueEntry>> {
        Box::pin(
            self.agents
                .start_stopped_restore(self, workspace, execution, env),
        )
    }

    fn stop_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        policy: StopPolicy,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.stop_executions(self, id, policy, request))
    }

    fn stop_space<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        request: StopRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.stop_space(self, selector, request))
    }

    fn stop_space_agent_executions<'a>(
        &'a self,
        id: &'a SpaceId,
        request: &'a StopRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.stop_space_agent_executions(self, id, request))
    }

    fn stop_space_manually<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        acknowledge_unproven: bool,
        launches: QueuedLaunches,
    ) -> BoxFuture<'a, Result<StoppedSpace>> {
        Box::pin(
            self.agents
                .stop_space_manually(self, selector, acknowledge_unproven, launches),
        )
    }

    fn take_chat_place<'a>(
        &'a self,
        chat: &'a SpaceId,
        place: Origin,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.take_chat_place(chat, place))
    }

    fn turn_ended<'a>(
        &'a self,
        id: &'a SpaceId,
        runs: &'a (dyn Fn(&RunningAgent) -> bool + Send + Sync),
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.agents.turn_ended(id, runs))
    }
}
