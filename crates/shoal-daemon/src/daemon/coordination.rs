//! What `shoal-coordination` needs of the rest of the daemon: the spaces it
//! routes between and their files, the agents and the queue, the callers
//! of scope tokens, the hub, the repositories, the reviews and the links,
//! and the records it reads and writes in its own transactions.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_coordination::daemon::{
    Agents, Attaching, Callers, Files, Hub, KeptFinding, Links, PassedUp, Records, Repositories,
    Reviews, Spaces,
};
use shoal_daemon_base::notifications::{Coordinators, Recorded, Source};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, ProposalDecision};
use shoal_protocol::config::permissions::Permissions;
use shoal_protocol::files::{Attacher, FileSource, NewFile, SentFile};
use shoal_protocol::hub::Member;
use shoal_protocol::hub::directory::HubView;
use shoal_protocol::hub::routing::ChainedMode;
use shoal_protocol::model::{Located, QueueEntry, Repository, Space, SpaceTarget, Workspace};
use shoal_protocol::protocol::{Body, Operation};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::threads::{Outcome, Thread};
use shoal_store::tree::TrackedAgents;

use crate::{
    config::{Config, Effective},
    daemon::{alerts::NewAlert, manager::Manager, scope::Caller, spaces::Removing},
    repository::Selector,
};

#[cfg(test)]
mod tests;

impl Coordinators for Manager {
    fn tell<'a>(
        &'a self,
        alert: NewAlert,
        for_user: bool,
        source: Source,
    ) -> BoxFuture<'a, Option<shoal_daemon_base::alerts::Sent>> {
        Box::pin(
            self.coordination
                .tell_coordinator(self, alert, for_user, source),
        )
    }
}

impl Spaces for Manager {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space(self, selector))
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space_by_id(self, id))
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(Manager::find_space(self, id))
    }

    fn space_anywhere<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Located>> {
        Box::pin(Manager::space_anywhere(self, selector))
    }

    fn space_id_anywhere<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        Box::pin(Manager::space_id_anywhere(self, selector))
    }

    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(Manager::spaces_within(self, root))
    }

    fn find_spaces<'a>(
        &'a self,
        target: &'a SpaceTarget,
        among: Vec<Space>,
    ) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(Manager::find_spaces(self, target, among))
    }

    fn being_removed(&self, space: &Space) -> bool {
        Manager::being_removed(self, space)
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace(self, selector))
    }

    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(Manager::find_workspace(self, id))
    }

    fn list_workspaces(&self) -> BoxFuture<'_, Result<Vec<Workspace>>> {
        Box::pin(Manager::list_workspaces(self))
    }

    fn chat_home<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(Manager::chat_home(self, id))
    }

    fn user_holds<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::user_holds(self, id))
    }

    fn hold_decision_guard<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Send>>>> {
        Box::pin(async move {
            let guard = Manager::hold_decision_guard(self, request, approve).await?;
            Ok(guard.map(|guard| Box::new(guard) as Box<dyn Send>))
        })
    }

    fn ensure_merge_current<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        approve: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::ensure_merge_current(self, request, approve))
    }
}

impl Files for Manager {
    fn attacher<'a>(&'a self, caller: Option<&'a Caller>) -> BoxFuture<'a, Attacher> {
        Box::pin(Manager::attacher(self, caller))
    }

    fn attach_files<'a>(
        &'a self,
        space: &'a Space,
        source: FileSource,
        message: &'a str,
        files: Vec<NewFile>,
        attacher: &'a Attacher,
    ) -> BoxFuture<'a, Result<Attaching>> {
        Box::pin(async move {
            let space = space.clone().into();
            Manager::attach_files(self, &space, source, message, files, attacher).await
        })
    }

    fn sent_files(&self, paths: Vec<PathBuf>) -> BoxFuture<'_, Result<Vec<SentFile>>> {
        Box::pin(Manager::sent_files(self, paths))
    }

    fn queue_with_files<'a>(
        &'a self,
        space: Space,
        alert: NewAlert,
        attachments: Vec<NewFile>,
        attacher: &'a Attacher,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(async move {
            let (id, text) = (space.id.clone(), alert.message.clone());
            self.queue_attached(space.into(), &text, attachments, |copies| {
                let alert = NewAlert {
                    attachments: copies.iter().map(|copy| copy.path.clone()).collect(),
                    ..alert
                };
                let attacher = attacher.clone();
                let record = Box::new(move |tx: &Transaction<'_>, id: &SpaceId| {
                    let source = FileSource::Message;
                    crate::daemon::workspace_files::record(tx, id, source, &attacher, &copies)
                });
                self.agents.queue_copied(&id, alert, record)
            })
            .await
        })
    }
}

impl Agents for Manager {
    fn tracked_agents(&self) -> BoxFuture<'_, TrackedAgents> {
        Box::pin(Manager::tracked_agents(self))
    }

    fn running_agent_workspaces(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        Box::pin(self.agents.running_agent_workspaces())
    }

    fn recorded_agent_spaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(self.agents.recorded_agent_spaces())
    }

    fn queued_agent_workspaces(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(self.agents.queued_agent_workspaces())
    }

    fn has_saved_agent(&self, id: &SpaceId) -> bool {
        self.agents.has_saved_agent(id)
    }

    fn queue_alert<'a>(
        &'a self,
        id: &'a SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(self.agents.queue_alert(id, alert, for_user))
    }

    fn queue_agent_message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.queue_agent_message(id, kind, message))
    }

    fn mark_messages_delivered<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        ids: Vec<i64>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.agents
                .mark_agent_messages_delivered(self, selector, ids),
        )
    }

    fn queue_entries(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(self.agents.queue_entries(self))
    }

    fn launch_parents(&self, entries: &[QueueEntry]) -> HashMap<QueueEntryId, SpaceId> {
        self.agents.launch_parents(entries)
    }

    fn started_launch<'a>(
        &'a self,
        entry: &'a QueueEntryId,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(self.agents.started_launch(entry))
    }
}

impl Callers for Manager {
    fn execution_caller<'a>(&'a self, token: &'a ScopeToken) -> BoxFuture<'a, Option<Caller>> {
        Box::pin(async move { self.agents.executions().scope(token).await })
    }

    fn operation_caller(&self, token: &ScopeToken) -> Option<Caller> {
        self.operations.caller(token)
    }
}

impl Hub for Manager {
    fn names_vouched_own(&self, caller: &Caller, selector: &SpaceSelector) -> bool {
        self.hub.names_vouched_own(caller, selector)
    }

    fn vouched_caller(&self, token: &ScopeToken) -> Option<Option<Caller>> {
        self.hub.vouched_caller(token)
    }

    fn runs_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(self.hub.runs_elsewhere(self, id))
    }

    fn ancestors_elsewhere<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(self.hub.ancestors_elsewhere(self, id))
    }

    fn reached_through_elsewhere<'a>(&'a self, root: &'a SpaceId) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(self.hub.reached_through_elsewhere(self, root))
    }

    fn reaches_elsewhere<'a>(
        &'a self,
        caller: &'a Caller,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.hub.reaches_elsewhere(self, caller, id))
    }

    fn deciders_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        online: bool,
    ) -> BoxFuture<'a, Vec<SpaceId>> {
        Box::pin(self.hub.deciders_elsewhere(self, id, online))
    }

    fn permissions_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Option<Permissions>>> {
        Box::pin(self.hub.permissions_elsewhere(self, id, repository))
    }

    fn reported_above<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<(Option<Permissions>, Vec<ChainedMode>)>>> {
        Box::pin(self.hub.reported_above(self, id))
    }

    fn repositories_view(&self) -> BoxFuture<'_, Result<Option<HubView>>> {
        Box::pin(self.hub.repositories_view(self))
    }

    fn acting_space<'a>(
        &'a self,
        caller: Option<&'a Caller>,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<(SpaceId, String)>> {
        Box::pin(self.hub.acting_space(self, caller, selector))
    }

    fn message_elsewhere<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.hub.message_elsewhere(self, id, kind, message))
    }

    fn ask_machine_or_hold<'a>(
        &'a self,
        machine: &'a Member,
        operation: Operation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Body>> {
        Box::pin(
            self.hub
                .ask_machine_or_hold(self, machine, operation, caller),
        )
    }

    fn pass_up<'a>(&'a self, above: SpaceId, event: PassedUp<'a>) -> BoxFuture<'a, Result<()>> {
        let event = crate::daemon::hub::Event {
            name: event.name,
            space: event.space,
            kind: event.kind,
            for_user: event.for_user,
            message: event.message,
            agent: event.agent,
            files: event.files,
        };
        Box::pin(self.hub.pass_up(self, above, event))
    }
}

impl Repositories for Manager {
    fn repository(&self, selector: Selector) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.repositories.repository(selector))
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.repositories.list())
    }

    fn space_settings_in<'a>(
        &'a self,
        id: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(self.repositories.space_settings_in(id))
    }

    fn space_settings_from<'a>(
        &'a self,
        config: &'a Config,
        space: &'a Space,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(
            self.repositories
                .space_settings_from(self, config, space, repository),
        )
    }
}

impl Reviews for Manager {
    fn answer_posts(&self) {
        self.reviews.answer_posts();
    }

    fn give_gate<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.reviews.give_gate(self, request))
    }

    fn keep_false_positive(
        &self,
        thread: Thread,
        outcome: Option<Outcome>,
    ) -> BoxFuture<'_, Result<Option<Box<dyn KeptFinding + '_>>>> {
        Box::pin(async move {
            let kept = (self.reviews)
                .keep_false_positive(self, &thread, outcome.as_ref())
                .await?;
            Ok(kept.map(|kept| Box::new(KeptFalsePositive(kept)) as Box<dyn KeptFinding + '_>))
        })
    }
}

/// A false positive the reviews keep for a resolution not recorded yet.
struct KeptFalsePositive<'a>(shoal_reviews::findings::KeptFalsePositive<'a>);

impl KeptFinding for KeptFalsePositive<'_> {
    fn undo<'a>(self: Box<Self>) -> BoxFuture<'a, Result<()>>
    where
        Self: 'a,
    {
        Box::pin(self.0.undo())
    }
}

impl Links for Manager {
    fn ensure_not_pushing(&self, request: &ApprovalRequest) -> Result<()> {
        self.links.ensure_not_pushing(self, request)
    }

    fn decide_proposal<'a>(
        &'a self,
        request: ApprovalRequest,
        approve: bool,
        decision: ProposalDecision,
        env: Option<BTreeMap<String, String>>,
        routed_to: Option<&'a SpaceId>,
    ) -> BoxFuture<'a, Result<ApprovalRequest>> {
        Box::pin(
            self.links
                .decide_proposal(self, request, approve, decision, env, routed_to),
        )
    }

    fn prune_proposal_files<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(self.links.prune_proposal_files(self, request))
    }
}

/// What coordination reads and writes of the spaces and agents in its own
/// transactions: the removals the daemon holds, the holds approvals place
/// and the alerts queued for agents.
pub(crate) struct CoordinationRecords(pub(crate) Removing);

impl Records for CoordinationRecords {
    fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        self.0.require_taking_work(db, id)
    }

    fn place_approved_hold(&self, tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
        crate::daemon::holds::place_approved(tx, request, &self.0)
    }

    fn agent_turn(
        &self,
        db: &Connection,
        space: &SpaceId,
    ) -> Result<(Option<i64>, Option<ExecutionId>)> {
        crate::daemon::turn_state::turn_of(db, space)
    }

    fn queue_alert_in(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        alert: &NewAlert,
    ) -> Result<Recorded> {
        let queued = crate::daemon::messages::queue_in(
            tx,
            &crate::daemon::agent_service::AgentRecords,
            id,
            alert,
        )?;
        Ok(Recorded {
            id: queued.id,
            new: queued.new,
        })
    }
}
