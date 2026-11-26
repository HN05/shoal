//! The hub, for the spaces: the spaces of other machines, moves and
//! takeovers, and the packages the hub keeps.
use std::path::Path;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::member_ids::MemberId;
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::directory::Keeps;
use shoal_protocol::hub::directory::{HubView, Listed};
use shoal_protocol::hub::packages::KeptPackage;
use shoal_protocol::hub::placement::Decision;
use shoal_protocol::hub::{Machine, Member};
use shoal_protocol::model::SpaceRef;
use shoal_protocol::protocol::{Body, Operation};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_schedule::placement::snapshot::Request;
use shoal_spaces::daemon::{BelowElsewhere, ClaimFailure};

use crate::daemon::{manager::Manager, scope::Caller};
use shoal_core::launches::SpaceRepository;

/// The spaces' claim failure the hub's `failure` is.
pub(super) fn claim_failure(failure: shoal_hub::ClaimFailure) -> ClaimFailure {
    match failure {
        shoal_hub::ClaimFailure::Refused(error) => ClaimFailure::Refused(error),
        shoal_hub::ClaimFailure::Unknown(error) => ClaimFailure::Unknown(error),
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
            crate::daemon::hub_service::sending::deliver_file(file, incoming, outgoing).await
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
