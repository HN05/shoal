//! What the spaces service needs of the rest of the daemon: the
//! implementations of its interface, which call the other subsystems and
//! convert between the spaces' types and theirs, and the records of the
//! other subsystems that the spaces read and write in their own
//! transactions.
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_coordination::Coordination;
use shoal_forge::transport::account::Account;
use shoal_forge_types::activity::status::PrStatus;
use shoal_forge_types::items::action::Action;
use shoal_forge_types::items::issue::IssueDetails;
use shoal_forge_types::items::link::{ItemKind, LinkKind, Selection};
use shoal_forge_types::items::reference::{ItemInput, ItemRef};
use shoal_forge_types::{ForgeRepo, PullRequest};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::member_ids::MemberId;
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_links::issue::assignees::AssignerOf;
use shoal_protocol::agent::Agent;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::cleanup::Emptying;
use shoal_protocol::containers::ContainerProject;
use shoal_protocol::executions::{ProxyRetry, TurnRetry};
use shoal_protocol::hub::directory::Keeps;
use shoal_protocol::hub::directory::{HubView, Listed};
use shoal_protocol::hub::packages::KeptPackage;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::hub::{Machine, Member};
use shoal_protocol::links::{Unlinked, issue, pr};
use shoal_protocol::model::{
    Assigner, Chat, CheckoutSync, Execution, LinkedItem, MergePath, MergeWarning, QueueAdmission,
    QueueEntry, QueuedJob, Relation, Repository, ReviewMark, RunningAgent, Selected, Space,
    SpaceLinks, SpaceRef, StoppedSpace, SyncedRepository, Workspace, WorkspaceRef,
};
use shoal_protocol::processes::Identity;
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::operations::{Ended, LaunchRequest, OperationSpec, QueuedLaunch};
use shoal_protocol::protocol::{
    Body, ConfigTarget, ItemContext, Operation, QueuedLaunches, RepoTarget,
};
use shoal_protocol::resources::ResourceLease;
use shoal_protocol::reviews::CarriedReview;
use shoal_protocol::sessions::Origin;
use shoal_protocol::sim::Simulator;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::state::{AgentState, BlockerKind};
use shoal_protocol::status::IssueStatus;
use shoal_refs::CommitId;
use shoal_schedule::placement::snapshot::Request;
use shoal_spaces::Spaces;
use shoal_spaces::daemon::{
    BelowElsewhere, CallerAgent, ClaimFailure, Dropped, Onto, OpenIssue, PushedMerge, Restack,
    ReviewGate,
};
use tokio::sync::{MutexGuard, OwnedMutexGuard};

use super::{
    alerts::AlertKind, coordination::CoordinationRecords, executions, idle_wakes, manager::Manager,
    messages, restarts, scope::Caller, turn_retry, turn_state, wake_timers,
};
use crate::config::repo::ConfigLayers;
use crate::config::{Config, Effective};
use crate::repository::Selector;
use shoal_agents::messages::Delivery;
use shoal_core::launches::SpaceRepository;
use shoal_daemon_base::blockers::Readiness;
use shoal_daemon_base::stops::{StopPolicy, StopRequest};
use shoal_spaces::target_checks::TargetCheck;

#[cfg(test)]
mod tests;

impl shoal_spaces::daemon::Daemon for Manager {
    fn spaces(&self) -> &Spaces {
        &self.spaces
    }

    fn coordination(&self) -> &Coordination {
        &self.coordination
    }
}

impl Manager {
    /// The space `space` as a request about its items sees it.
    async fn link_space_of(&self, space: &Space) -> Result<shoal_links::space::LinkSpace> {
        (self.links)
            .link_space(self, &SpaceSelector::id(&space.id))
            .await
    }
}

/// The spaces' claim failure the hub's `failure` is.
fn claim_failure(failure: shoal_hub::ClaimFailure) -> ClaimFailure {
    match failure {
        shoal_hub::ClaimFailure::Refused(error) => ClaimFailure::Refused(error),
        shoal_hub::ClaimFailure::Unknown(error) => ClaimFailure::Unknown(error),
    }
}

/// The reviews' gate the spaces' `gate` is.
fn review_gate(gate: ReviewGate) -> shoal_reviews::required::Gate {
    match gate {
        ReviewGate::Ready => shoal_reviews::required::Gate::Ready,
        ReviewGate::Merge => shoal_reviews::required::Gate::Merge,
    }
}

impl shoal_spaces::daemon::Operations for Manager {
    fn start_operation<'a>(
        &'a self,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(Manager::start_operation(self, spec, env, caller))
    }

    fn spawn_operation<'a>(
        &'a self,
        workspace: SpaceId,
        spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(Manager::spawn_operation(self, workspace, spec, env, caller))
    }

    fn operation_runs_in(&self, id: &SpaceId) -> bool {
        Manager::operation_runs_in(self, id)
    }

    fn operation_over<'a>(&'a self, id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            match self.operation(id, None).await {
                Ok(entry) => entry.events_from(u64::MAX).1.is_some(),
                Err(_) => true,
            }
        })
    }

    fn operation_ending<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Option<Option<Ended>>> {
        Box::pin(async move {
            let entry = self.operation(id, None).await.ok()?;
            Some(entry.ending())
        })
    }

    fn operation_ended<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Option<Ended>> {
        Box::pin(async move {
            let entry = self.operation(id, None).await.ok()?;
            let mut changed = entry.watch();
            loop {
                if let (_, Some(ended)) = entry.events_from(u64::MAX) {
                    return Some(ended);
                }
                changed.changed().await.ok()?;
            }
        })
    }

    fn target_check(&self, operation: &Operation) -> Option<TargetCheck> {
        super::target_checks::Checked::target_check(operation)
    }

    fn fakes_workers(&self) -> bool {
        super::fake_workers::faked()
    }

    fn failure(&self, error: &anyhow::Error) -> Body {
        super::answer::failure(error)
    }
}

impl shoal_spaces::daemon::Links for Manager {
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(shoal_links::pr::current_head(workspace))
    }

    fn acting_repository<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<RepositoryId>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Ok(space.repository_id().clone())
        })
    }

    fn origin_path<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Ok(space.origin_path().to_owned())
        })
    }

    fn read_pull_title<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.links.read_pull_title(checkout, input))
    }

    fn branch_merge_warning<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<MergeWarning>>> {
        Box::pin(Manager::branch_merge_warning(self, selector))
    }

    fn space_forge<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            shoal_links::forges::of_space(&self.forges, &space).await
        })
    }

    fn linked_states<'a>(
        &'a self,
        remote: &'a Result<Option<String>>,
        pr_cleanup: Option<&'a pr::Registration>,
        issues: &'a [issue::Registration],
    ) -> BoxFuture<'a, (Vec<PrStatus>, Vec<IssueStatus>)> {
        Box::pin(Manager::linked_states(self, remote, pr_cleanup, issues))
    }

    fn acting_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Manager::acting_settings(self, &space).await
        })
    }

    fn settle_agents(&self, id: &SpaceId) {
        self.links.agent_settles().request(id)
    }

    fn apply_own<'a>(
        &'a self,
        id: &'a SpaceId,
        forge: &'a ForgeRepo,
        account: &'a Account,
        item: (shoal_forge_types::items::link::ItemKind, u64, String),
        action: &'a Action,
    ) -> BoxFuture<'a, Result<()>> {
        let (kind, number, url) = item;
        let target = shoal_links::actions::Target { kind, number, url };
        Box::pin(async move {
            (self
                .links
                .apply_own(self, id, forge, account, target, action, None))
            .await?;
            Ok(())
        })
    }

    fn check_conventions<'a>(
        &'a self,
        space: &'a Space,
        settings: &'a Effective,
        marks: &'a mut [ReviewMark],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Ok(space) = self.link_space_of(space).await else {
                return;
            };
            self.links.check_conventions(&space, settings, marks).await
        })
    }

    fn explicit_item<'a>(
        &'a self,
        checkout: &'a Path,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Option<ItemRef>>> {
        Box::pin(self.links.explicit_item(checkout, selection))
    }

    fn follow_pushed_merge(&self, pushed: PushedMerge) -> BoxFuture<'_, Result<()>> {
        let PushedMerge {
            space,
            remotes,
            branch,
            commit,
        } = pushed;
        let pushed = shoal_links::wait::merge_lookups::PushedMerge {
            space,
            remotes,
            branch,
            commit,
        };
        Box::pin(self.links.follow_pushed_merge(self, pushed))
    }

    fn hand_over_assignees<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.links.hand_over_assignees(id))
    }

    fn issue_blockers<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Blocker>>> {
        Box::pin(self.links.issue_blockers(self, context, item, caller))
    }

    fn issue_details<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        closed: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<IssueDetails>> {
        Box::pin(
            self.links
                .issue_details(self, context, item, closed, caller),
        )
    }

    fn issue_forge<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<(ForgeRepo, ItemRef)>> {
        Box::pin(self.links.issue_forge(checkout, input))
    }

    fn issue_registrations<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<issue::Registration>>> {
        Box::pin(self.links.issue_registrations(id))
    }

    fn lock_change_gates<'a>(
        &'a self,
        id: &'a SpaceId,
        others: &'a [&'a SpaceId],
    ) -> BoxFuture<'a, Result<shoal_daemon_base::gates::PrGuard>> {
        Box::pin(self.links.lock_change_gates(id, others))
    }

    fn awaits_merge_runs<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.links.awaits_merge_runs(id))
    }

    fn merged_unseen<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.links.merged_unseen(self, id))
    }

    fn merge_path<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<MergePath>> {
        Box::pin(self.links.merge_path(self, selector))
    }

    fn pr_registration<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<pr::Registration>>> {
        Box::pin(self.links.pr_registration(id))
    }

    fn pull_request<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<PullRequest>> {
        Box::pin(self.links.pull_request(self, context, item, caller))
    }

    fn read_open_issue<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<OpenIssue>> {
        Box::pin(async move {
            let shoal_links::reads::checkouts::OpenIssue { url, title, labels } =
                self.links.read_open_issue(checkout, input).await?;
            Ok(OpenIssue { url, title, labels })
        })
    }

    fn release_assignees(&self, dropped: Vec<Dropped>) -> BoxFuture<'_, Vec<String>> {
        let dropped = (dropped.into_iter())
            .map(|Dropped { url, login }| shoal_links::issue::assignees::Dropped { url, login })
            .collect();
        Box::pin(self.links.release_assignees(dropped))
    }

    fn relink_space<'a>(
        &'a self,
        id: &'a SpaceId,
        links: &'a SpaceLinks,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(self.links.relink_space(self, id, links, caller))
    }

    fn restack<'a>(
        &'a self,
        stacked: &'a WorkspaceRef,
        restack: &'a Restack<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        let restack = shoal_links::stack::Restack {
            base: restack.base,
            head: restack.head,
            merged_heads: restack.merged_heads,
            target: restack.target,
            onto: match restack.onto {
                Onto::Remote => shoal_links::stack::Onto::Remote,
                Onto::Local => shoal_links::stack::Onto::Local,
                Onto::Held => shoal_links::stack::Onto::Held,
            },
            new_base: restack.new_base.clone(),
            cause: restack.cause,
            account: restack.account,
        };
        Box::pin(async move { self.links.restack(self, stacked, &restack).await })
    }

    fn select_context_items<'a>(
        &'a self,
        target: &'a RepoTarget,
        selection: &'a Selection,
        elsewhere: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(
            self.links
                .select_context_items(self, target, selection, elsewhere, caller),
        )
    }

    fn selected_items<'a>(
        &'a self,
        space: &'a Space,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            self.links.selected_items(&space, selection).await
        })
    }

    fn set_draft<'a>(
        &'a self,
        space: &'a Space,
        urls: &'a [ItemRef],
        draft: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            self.links.set_draft(self, &space, urls, draft).await
        })
    }

    fn set_issue<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        input: &'a ItemInput,
        title: Option<String>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(
            self.links
                .set_issue(self, selector, input, title, Assigner::of(caller)),
        )
    }

    fn set_pr<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        action: pr::Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.links.set_pr(self, selector, action))
    }

    fn settle_removed_agents<'a>(
        &'a self,
        space: &'a Space,
        issues: &'a [ItemRef],
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(self.links.settle_removed_agents(self, space, issues))
    }

    fn unlink_items<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&'a ItemInput>,
    ) -> BoxFuture<'a, Result<Unlinked>> {
        Box::pin(
            self.links
                .unlink_items(self, selector, relation, kind, input),
        )
    }
}

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
            Ok(saved.execution_ids().into_iter().collect())
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

impl shoal_spaces::daemon::Repositories for Manager {
    fn apply_seed<'a>(
        &'a self,
        repo: &'a Repository,
        workspace: &'a Workspace,
        settings: &'a Effective,
    ) -> BoxFuture<'a, ()> {
        Box::pin(
            self.repositories
                .apply_seed(self, repo, workspace, settings),
        )
    }

    fn checkout_sync(&self, repository_id: &RepositoryId) -> Option<CheckoutSync> {
        self.repositories.checkout_sync(repository_id)
    }

    fn destroy_seed_outputs<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(self.repositories.destroy_seed_outputs(self, workspace))
    }

    fn ensure_available<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.repositories.ensure_available(id))
    }

    fn fetch_default_branch<'a>(
        &'a self,
        repository_id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.repositories.fetch_default_branch(self, repository_id))
    }

    fn default_branch_fresh<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, bool> {
        Box::pin(self.repositories.default_branch_fresh(self, id))
    }

    fn empty_caches<'a>(
        &'a self,
        repo: &'a Repository,
        dry_run: bool,
    ) -> BoxFuture<'a, Result<Option<Emptying>>> {
        Box::pin(self.repositories.empty_caches(repo, dry_run))
    }

    fn forget_rejected_config(&self, id: &SpaceId) {
        self.repositories.forget_rejected_config(id)
    }

    fn lock_repository(
        &self,
        selector: Selector,
    ) -> BoxFuture<'_, Result<(Repository, OwnedMutexGuard<()>)>> {
        Box::pin(self.repositories.lock(selector))
    }

    fn verified_seed_worktree<'a>(
        &'a self,
        repo: &'a Repository,
        root: &'a Path,
    ) -> BoxFuture<'a, Option<PathBuf>> {
        Box::pin(async move {
            let seed = shoal_repositories::seed::Layout::new(root, &repo.id);
            let verified = shoal_repositories::seed::is_seed_worktree(repo, &seed).await;
            verified.unwrap_or(false).then(|| seed.worktree())
        })
    }

    fn prepare_chat_root(&self) -> BoxFuture<'_, Result<PathBuf>> {
        Box::pin(self.repositories.prepare_chat_root())
    }

    fn register_repository(
        &self,
        source: Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.repositories.register(source, name, clone_path))
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings(id))
    }

    fn repository_settings_with_templates<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.repository_settings_with_templates(id))
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings(self, space))
    }

    fn space_settings_with_templates<'a>(
        &'a self,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .space_settings_with_templates(self, space, repository),
        )
    }

    fn sync_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(self.repositories.sync_repository(self, selector))
    }

    fn target_settings_with_templates(
        &self,
        target: ConfigTarget,
    ) -> BoxFuture<'_, Result<Effective>> {
        Box::pin(
            self.repositories
                .target_settings_with_templates(self, target),
        )
    }

    fn tracked_config<'a>(&'a self, repository: &'a Repository) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.repositories.tracked_config(repository))
    }

    fn tracked_settings(&self, tracked: &str) -> Result<Effective> {
        self.repositories.tracked_settings(tracked)
    }

    fn workspace_layers<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        Box::pin(self.repositories.workspace_layers(self, workspace))
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.workspace_settings(self, workspace))
    }

    fn workspace_settings_from<'a>(
        &'a self,
        config: &'a Config,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_from(self, config, workspace),
        )
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .workspace_settings_with_templates(self, workspace),
        )
    }
}

impl shoal_spaces::daemon::Hub for Manager {
    fn deliver_file<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
        file: &'a Path,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(async move {
            let opened = self.hub.open_duplex(self, machine, operation, caller);
            let (incoming, outgoing) = opened.await?;
            super::hub::sending::deliver_file(file, incoming, outgoing).await
        })
    }

    fn claim_moved<'a>(
        &'a self,
        id: &'a SpaceId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<(), ClaimFailure>> {
        Box::pin(async move {
            self.hub
                .claim_moved(self, id, ticket)
                .await
                .map_err(claim_failure)
        })
    }

    fn claim_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.hub.claim_space(self, id))
    }

    fn claim_taken_over<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<(), ClaimFailure>> {
        Box::pin(async move {
            self.hub
                .claim_taken_over(self, id)
                .await
                .map_err(claim_failure)
        })
    }

    fn coordinator_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(self.hub.coordinator_elsewhere(self, id))
    }

    fn descendants_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<SpaceRef>>> {
        Box::pin(self.hub.descendants_elsewhere(self, id))
    }

    fn ensure_parent_elsewhere<'a>(
        &'a self,
        space: Option<&'a SpaceId>,
        repository: Option<&'a RepositoryId>,
        parent: &'a SpaceId,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.hub
                .ensure_parent_elsewhere(self, space, repository, parent),
        )
    }

    fn fetch_package<'a>(&'a self, id: &'a SpaceId, file: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.hub.fetch_package(self, id, file))
    }

    fn hub_view<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Option<HubView>>> {
        Box::pin(self.hub.hub_view(self, caller))
    }

    fn kept_package<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<KeptPackage>>> {
        Box::pin(self.hub.kept_package(self, id))
    }

    fn listed<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.hub.listed(id))
    }

    fn machine_name(&self) -> BoxFuture<'_, String> {
        Box::pin(self.hub.machine_name(self))
    }

    fn other_machines(&self) -> BoxFuture<'_, Vec<Member>> {
        Box::pin(self.hub.other_machines(self))
    }

    fn waiting_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, usize> {
        Box::pin(self.hub.waiting_elsewhere(self, id))
    }

    fn tell_machine<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.hub.tell_machine(self, machine, operation))
    }

    fn machine_of<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<(Member, Listed)>>> {
        Box::pin(self.hub.machine_of(self, selector))
    }

    fn record_keeper<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Member>>> {
        Box::pin(self.hub.keeper_of(self, selector, Keeps::Record))
    }

    fn named_machine<'a>(&'a self, named: &'a str) -> BoxFuture<'a, Result<(Member, bool)>> {
        Box::pin(async move {
            let named = self.hub.named_machine(self, named).await?;
            Ok((named.machine, named.own))
        })
    }

    fn files_stay_here(&self, elsewhere: &str) -> anyhow::Error {
        shoal_hub::files_stay_here(elsewhere)
    }

    fn never_sent(&self, error: &anyhow::Error) -> bool {
        error.is::<shoal_hub::session::Unsent>()
    }

    fn origin_url_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.hub.origin_url_elsewhere(self, id))
    }

    fn parent_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceRef>>> {
        Box::pin(self.hub.parent_elsewhere(self, id))
    }

    fn place_start(&self, request: Request) -> BoxFuture<'_, Result<Option<Decision>>> {
        Box::pin(self.hub.place(self, request))
    }

    fn refuse_live_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.hub.refuse_live_elsewhere(self, id))
    }

    fn remove_children_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        below: BelowElsewhere<'a>,
    ) -> BoxFuture<'a, Result<Vec<SpaceName>>> {
        let below = shoal_hub::BelowElsewhere {
            refuse_holds: below.refuse_holds,
            confirmed: below.confirmed,
            cause: below.cause,
        };
        Box::pin(self.hub.remove_children_elsewhere(self, id, below))
    }

    fn repository_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<SpaceRepository>> {
        Box::pin(self.hub.repository_elsewhere(self, id))
    }

    fn settle_move<'a>(
        &'a self,
        id: &'a SpaceId,
        machine: &'a MemberId,
        ticket: &'a MoveTicket,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.hub.settle_move(self, id, machine, ticket))
    }

    fn this_machine(&self) -> BoxFuture<'_, Result<Option<Machine>>> {
        Box::pin(self.hub.this_machine(self))
    }
}

impl shoal_spaces::daemon::Runner for Manager {
    fn attach_jobs<'a>(&'a self, spaces: &'a mut [Space]) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.runner.attach_jobs(self, spaces))
    }

    fn cancel_job_before_stopping<'a>(
        &'a self,
        id: &'a SpaceId,
        why: shoal_daemon_base::jobs::JobStop,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.runner.cancel_job_before_stopping(self, id, why).await;
        })
    }

    fn linked_job_workspaces<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(self.runner.linked_job_workspaces(self, id))
    }

    fn linked_waiting_jobs<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(self.runner.linked_waiting_jobs(self, id))
    }

    fn workspace_job<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<Box<QueuedJob>>>> {
        Box::pin(self.runner.workspace_job(self, id))
    }
}

impl shoal_spaces::daemon::Reviews for Manager {
    fn check_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.reviews
                .check(self, workspace, review_gate(gate), head, caller),
        )
    }

    fn require_review<'a>(
        &'a self,
        workspace: &'a Workspace,
        gate: ReviewGate,
        head: &'a CommitId,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.reviews
                .require(self, workspace, review_gate(gate), head, caller),
        )
    }

    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async move {
            let queued = self
                .reviews
                .queue_unreviewed(self, Err::<(), _>(error))
                .await;
            queued.map_or_else(
                |error| error,
                |()| anyhow::anyhow!("a refusal took no review"),
            )
        })
    }

    fn ensure_gate_hears<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_gate_hears(self, request))
    }

    fn pending_review<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<CarriedReview>>> {
        Box::pin(self.reviews.pending(id))
    }

    fn queue_carried_review<'a>(
        &'a self,
        id: &'a SpaceId,
        review: CarriedReview,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.queue_carried(self, id, review))
    }

    fn stop_reviews<'a>(&'a self, workspace_id: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(self.reviews.stop_in_workspace(self, workspace_id))
    }
}

impl shoal_spaces::daemon::Leases for Manager {
    fn container_project<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ContainerProject>>> {
        Box::pin(async move {
            let recorded = self.leases.container_project(id).await?;
            Ok(recorded.map(|recorded| recorded.project))
        })
    }

    fn list_resources<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ResourceLease>>> {
        Box::pin(self.leases.list_resources(self, selector))
    }

    fn list_simulators<'a>(
        &'a self,
        owner: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<Vec<Simulator>>> {
        Box::pin(self.leases.list_simulators(owner))
    }

    fn remove_containers<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.leases.remove_containers(workspace))
    }

    fn remove_simulators<'a>(&'a self, owner: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.leases.remove_simulators(owner))
    }

    fn kept_leases<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<shoal_protocol::model::KeptLease>>> {
        Box::pin(self.leases.kept_by(self, id))
    }

    fn run_resource_release_hook<'a>(
        &'a self,
        workspace: &'a Workspace,
        lease: &'a ResourceLease,
        command: Option<&'a Path>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.leases
                .run_resource_release_hook(self, workspace, lease, command),
        )
    }
}

impl shoal_spaces::daemon::Demos for Manager {
    fn stop_demos<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        name: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.demos.stop(self, selector, name))
    }
}

/// The records of the daemon's other subsystems, as the spaces read and
/// write them in their own transactions.
pub(crate) struct SpaceRecords {
    pub(crate) coordination: CoordinationRecords,
}

impl shoal_spaces::daemon::Records for SpaceRecords {
    fn load_workspace(&self, db: &Connection, workspace: &mut Workspace) -> Result<()> {
        let id = &workspace.id.clone();
        workspace.latest_review =
            shoal_reviews::store::latest_status(db, &super::reviews::ReviewRecords, id)?;
        workspace.agent_state = turn_state::current(db, id)?;
        workspace.sleep = idle_wakes::sleep::current(db, id)?;
        workspace.wake_at = wake_timers::due_at(db, id)?;
        workspace.asleep = super::agent_queue::asleep::current(db, id)?;
        workspace.restart = restarts::record::current(db, id)?;
        workspace.links = shoal_links::linked::space_links(db, id)?;
        Ok(())
    }

    fn load_chat(&self, db: &Connection, chat: &mut Chat) -> Result<()> {
        let id = &chat.id.clone();
        chat.links = shoal_links::linked::space_links(db, id)?;
        chat.agent_state = turn_state::current(db, id)?;
        chat.sleep = idle_wakes::sleep::current(db, id)?;
        chat.wake_at = wake_timers::due_at(db, id)?;
        chat.asleep = super::agent_queue::asleep::current(db, id)?;
        chat.restart = restarts::record::current(db, id)?;
        Ok(())
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
        let assignees = (super::links::issue::removed_assignees(tx, id)?.into_iter())
            .map(|dropped| Dropped {
                url: dropped.url,
                login: dropped.login,
            })
            .collect();
        Ok((assignees, super::links::issue::removed_issues(tx, id)?))
    }

    fn drop_pending_reviews(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        shoal_reviews::written::drop_pending_of_removed(tx, &super::reviews::ReviewRecords, id)
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
        super::hub::numbering::next(tx, id)
    }

    fn ensure_holders_fit(&self, tx: &Transaction<'_>, paths: &[PathBuf]) -> Result<()> {
        messages::ensure_holders_fit(tx, paths)
    }

    fn coordination(&self) -> &dyn shoal_coordination::daemon::Records {
        &self.coordination
    }
}
