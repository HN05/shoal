//! What the hub needs of the rest of the daemon, which [`Manager`] gives
//! it: the spaces it reports, routes to and tells about, their
//! permissions, the messages that reach their agents, the approvals and
//! threads that pass between machines, the app store a member asks of
//! the hub, and the global config the hub's copies of the shared files
//! change.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_daemon_base::alerts::{AlertKind, NewAlert};
use shoal_hub::daemon::{
    Approvals, Apps, Dispatch, GlobalConfig, Messages, Packages, Receiver, Records, Registrations,
    SpacePermissions, Spaces,
};

use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    approvals::ApprovalRequest,
    config::permissions::Permissions,
    hub::{Member, routing::ChainedMode},
    model::{QueueEntry, Repository, RunningAgent, Space},
    protocol::{AppMethod, Request},
    saved::SavedSummary,
    space_names::{SpaceName, SpaceSelector},
};
use shoal_schedule::priority::Priority;

use super::{approvals, manager::Manager, messages, scope::Caller, tree};

pub(crate) use shoal_hub::{Elsewhere, Event, Incoming, Named, Outgoing, Unanswered, numbering};

impl Spaces for Manager {
    fn space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        Box::pin(self.space_by_id(id))
    }

    fn select_space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        Box::pin(Manager::space(self, selector))
    }

    fn watch_agents(&self) -> tokio::sync::watch::Receiver<u64> {
        self.signals.watch_agents()
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(Manager::find_space(self, id))
    }

    fn listed_spaces<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(Manager::listed_spaces(self, caller))
    }

    fn spaces_within<'a>(&'a self, root: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(Manager::spaces_within(self, root))
    }

    fn priority<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Priority>> {
        Box::pin(self.space_priority(id))
    }

    fn reach_root<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(Manager::reach_root(self, caller))
    }

    fn coordinator<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async move {
            let (id, agents) = (id.clone(), self.tracked_agents().await);
            (self.store)
                .run(move |db| tree::coordinator_of(db, &id, &agents))
                .await
        })
    }

    fn receiver_from<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Receiver>>> {
        Box::pin(async move {
            let (id, agents) = (id.clone(), self.tracked_agents().await);
            let found = (self.store)
                .run(move |db| tree::receiver_from(db, &id, &agents))
                .await?;
            Ok(found.map(|found| match found {
                tree::Receiver::Here(id) => Receiver::Here(id),
                tree::Receiver::Elsewhere(id) => Receiver::Elsewhere(id),
            }))
        })
    }

    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>> {
        Box::pin(self.agents.running_agents())
    }

    fn with_agents(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async move {
            let agents = self.tracked_agents().await;
            (self.store)
                .run(move |db| tree::with_agents(db, &agents))
                .await
        })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(self.repositories.list())
    }

    fn acts_on_global_config(&self) -> bool {
        Manager::acts_on_global_config(self)
    }
}

impl SpacePermissions for Manager {
    fn caller_permissions<'a>(
        &'a self,
        caller: &'a Caller,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        Box::pin(Manager::caller_permissions(self, caller, repository))
    }

    fn chained_permissions<'a>(
        &'a self,
        chain: &'a [ChainedMode],
        repository: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Permissions>> {
        Box::pin(Manager::chained_permissions(self, chain, repository))
    }

    fn chat_modes<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Vec<ChainedMode>>> {
        Box::pin(Manager::chat_modes(self, space))
    }

    fn space_permissions<'a>(
        &'a self,
        id: &'a SpaceId,
        repository: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        Box::pin(Manager::space_permissions(self, id, repository))
    }
}

impl Messages for Manager {
    fn message<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.agents.queue_agent_message(id, kind, message))
    }

    fn message_next_turn<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, Result<()>> {
        let next_turn = messages::Delivery::NextTurn;
        Box::pin(
            self.agents
                .queue_agent_message_delivered(id, kind, message, next_turn),
        )
    }

    fn alert<'a>(
        &'a self,
        id: &'a SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.agents.queue_alert(id, alert, for_user).await.map(drop) })
    }
}

impl Approvals for Manager {
    fn approval_requests<'a>(
        &'a self,
        selector: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ApprovalRequest>>> {
        Box::pin(Manager::approval_requests(self, selector))
    }

    fn open_threads(&self) -> BoxFuture<'_, Result<Vec<shoal_protocol::threads::Thread>>> {
        Box::pin(Manager::open_threads(self))
    }

    fn pass_over_offline<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::pass_over_offline(self, request))
    }

    fn pass_to_user<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(Manager::pass_to_user(self, request))
    }

    fn saved_summaries<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<SavedSummary>>> {
        Box::pin(self.agents.saved_summaries(self, caller))
    }

    fn visible_queue(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(Manager::visible_queue(self))
    }
}

impl Dispatch for Manager {
    fn answer(&self, request: Request) -> BoxFuture<'_, Result<Vec<serde_json::Value>>> {
        Box::pin(socket::answer(&self.paths.socket, request))
    }

    fn stream<'a>(
        &'a self,
        request: Request,
        out: &'a mut Outgoing,
        up: Option<Incoming>,
        started: Box<dyn FnOnce() + Send + 'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(socket::stream(
            &self.paths.socket,
            request,
            out,
            up,
            started,
        ))
    }
}

impl Apps for Manager {
    fn serve_app_request(
        self: Arc<Self>,
        member: &Member,
        method: AppMethod,
        data: Option<String>,
        out: Outgoing,
    ) -> Result<()> {
        Manager::serve_app_request(&self, member, method, data, out)
    }
}

impl Packages for Manager {
    fn package_state<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<String>> {
        Box::pin(Manager::package_state(self, space))
    }

    fn write_package<'a>(
        &'a self,
        space: &'a Space,
        file: &'a std::path::Path,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.write_kept_package(space, file.to_owned()).await?;
            Ok(())
        })
    }

    fn retire_space<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<String>> {
        Box::pin(Manager::retire_space(self, space))
    }
}

impl shoal_hub::daemon::Queue for Manager {
    fn queue_room(&self) -> BoxFuture<'_, Result<shoal_schedule::placement::snapshot::Capacity>> {
        Box::pin(self.agents.queue_room(self))
    }
}

impl Registrations for Manager {
    fn register_clone(&self, url: String, name: String) -> BoxFuture<'_, Result<Repository>> {
        Box::pin(self.repositories.register_clone(url, name))
    }

    fn share_repository_as<'a>(
        &'a self,
        id: &'a RepositoryId,
        url: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.repositories.share_as(id, url).await.map(drop) })
    }

    fn local_config_text<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(self.repositories.local_config_text(id))
    }

    fn take_local_config_copy<'a>(
        &'a self,
        id: &'a RepositoryId,
        expected: Option<String>,
        text: Option<String>,
        keep: bool,
    ) -> BoxFuture<'a, Result<Option<std::path::PathBuf>>> {
        Box::pin((self.repositories).take_local_config_copy(self, id, expected, text, keep))
    }
}

impl GlobalConfig for Manager {
    fn reload_config(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.reload_config_and_recheck())
    }
}

/// The spaces' records, as the hub reads and renumbers them in its own
/// connections and transactions.
pub(crate) struct HubRecords;

impl Records for HubRecords {
    fn parent_id(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
        tree::parent_id(db, id)
    }

    fn parent_ids(&self, db: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
        tree::parent_ids(db)
    }

    fn below_elsewhere(&self, db: &Connection) -> Result<Vec<(SpaceId, SpaceId)>> {
        tree::below_elsewhere(db)
    }

    fn first_absent_above(&self, db: &Connection, id: &SpaceId) -> Result<Option<SpaceId>> {
        tree::first_absent_above(db, id)
    }

    fn reserved_numbers(&self, db: &Connection) -> Result<Vec<SpaceNumber>> {
        let mut statement = db.prepare(
            "SELECT DISTINCT number FROM space_numbers
                WHERE number NOT IN (SELECT number FROM spaces WHERE number IS NOT NULL)
                ORDER BY number",
        )?;
        let numbers = statement.query_map([], |row| row.get(0))?;
        Ok(numbers.collect::<rusqlite::Result<_>>()?)
    }

    fn space_number(
        &self,
        db: &Connection,
        id: &SpaceId,
    ) -> Result<Option<(SpaceName, SpaceNumber)>> {
        Ok(db
            .query_row("SELECT name, number FROM spaces WHERE id=?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?)
    }

    fn renumber_space(
        &self,
        tx: &Transaction<'_>,
        id: &SpaceId,
        from: SpaceNumber,
        to: SpaceNumber,
    ) -> Result<()> {
        // Its threads follow its number.
        tx.execute("UPDATE spaces SET number=?2 WHERE id=?1", params![id, to])?;
        approvals::renumber(tx, id, from, to)
    }
}

pub(crate) mod sending;
mod socket;
#[cfg(test)]
mod tests;
