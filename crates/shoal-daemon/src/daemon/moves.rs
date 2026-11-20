//! Moves the daemon runs as operations: started on the machine whose
//! spaces they take, by a worker that checks the spaces, packs them,
//! delivers the packages and ends the spaces here once their destination
//! has them, so a move goes on when the caller goes.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use super::{manager::Manager, scope::Caller};
use crate::protocol::operations::OperationSpec;
use crate::transfer::{MoveDestination, MoveOrder};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Start `order` in a worker that acts with `caller`'s scope, in `env`
    /// when given, returning the operation's ID, once every space it takes
    /// is here: live, or for a file a removed one. A space another machine
    /// has refuses it, naming that space, which the hub forwards it for.
    pub(crate) async fn start_move(
        &self,
        order: MoveOrder,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<String> {
        ensure!(!order.spaces.is_empty(), "a move takes at least one space");
        for ordered in &order.spaces {
            self.ensure_movable(&ordered.space, &order.destination)
                .await?;
        }
        let first = order.spaces[0].space.clone();
        let spec = OperationSpec::Move {
            order: Box::new(order),
        };
        self.spawn_operation(first, spec, env, caller).await
    }

    /// Refuse a space `id` that is not here to move to `destination`: a
    /// file takes a removed space too.
    async fn ensure_movable(&self, id: &SpaceId, destination: &MoveDestination) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let absent = match self.space(&selector).await {
            Ok(_) => return Ok(()),
            Err(absent) => absent,
        };
        match destination {
            MoveDestination::Ssh(_) => Err(absent),
            _ => self
                .removed_space(&selector)
                .await
                .map(drop)
                .or(Err(absent)),
        }
    }

    /// Refuse a scoped `caller` a move of a space its scope does not
    /// reach, or one another machine sent, since a move reaches outside the
    /// hub's trust.
    pub(crate) async fn confine_move(&self, caller: &Caller, order: &MoveOrder) -> Result<()> {
        ensure!(
            caller.vouched.is_none(),
            "only the user moves spaces from another machine"
        );
        for ordered in &order.spaces {
            let selector = SpaceSelector::id(&ordered.space);
            match self.space(&selector).await {
                Ok(space) => self.ensure_reaches(caller, &space).await?,
                Err(_) => self.ensure_restorable_by(&selector, caller).await?,
            }
        }
        Ok(())
    }
}
