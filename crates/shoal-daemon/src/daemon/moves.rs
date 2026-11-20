//! Moves the daemon runs as operations: started on the machine whose
//! spaces they take, by a worker that checks the spaces, packs them,
//! delivers the packages and ends the spaces here once their destination
//! has them, so a move goes on when the caller goes. A recorded move whose
//! worker went, as when the machine restarted during it, finishes in a
//! worker of its own, which tells the user how it ended.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use super::{log, manager::Manager, notifications::AlertKind, scope::Caller};
use crate::protocol::operations::{Ended, OperationSpec, Outcome};
use crate::transfer::{MoveDestination, MoveLock, MoveOrder, MoveReport};
use shoal_protocol::space_names::SpaceSelector;

/// How often the daemon looks for recorded moves whose worker went.
const INTERRUPTED_MOVES_INTERVAL: Duration = Duration::from_secs(60);

/// Finish the recorded moves whose worker went, at startup and then as
/// often as [`INTERRUPTED_MOVES_INTERVAL`] says.
pub(crate) async fn finish_interrupted(manager: Arc<Manager>) {
    loop {
        {
            let _pass = manager.gates.background_pass().await;
            if let Err(error) = manager.finish_interrupted_moves().await {
                log!("finishing interrupted moves: {error:#}");
            }
        }
        tokio::time::sleep(INTERRUPTED_MOVES_INTERVAL).await;
    }
}

impl Manager {
    /// Start `order` in a worker that acts with `caller`'s scope, in `env`
    /// when given, returning the operation's ID, once every space it takes
    /// is here: live, or for a file a removed one. A space another machine
    /// has refuses it, naming that space, which the hub forwards it for, as
    /// does a joined machine to move to that is this one or offline.
    pub(crate) async fn start_move(
        &self,
        order: MoveOrder,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<String> {
        ensure!(!order.spaces.is_empty(), "a move takes at least one space");
        for ordered in &order.spaces {
            self.ensure_movable(ordered, &order.destination).await?;
        }
        if let MoveDestination::Machine(destination) = &order.destination {
            self.receiving_machine(&destination.machine).await?;
        }
        let first = order.spaces[0].clone();
        let spec = OperationSpec::Move {
            order: Box::new(order),
        };
        self.spawn_operation(first, spec, env, caller).await
    }

    /// Refuse a space `id` that is not here to move to `destination`: a
    /// file takes a removed space too, and only a move to another machine
    /// goes to the machine that has the space, since a file would land
    /// there rather than where the caller named it.
    async fn ensure_movable(&self, id: &SpaceId, destination: &MoveDestination) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let absent = match self.space(&selector).await {
            Ok(_) => return Ok(()),
            Err(absent) => absent,
        };
        if destination.machine().is_some() {
            return Err(absent);
        }
        match self.removed_space(&selector).await {
            Ok(_) => Ok(()),
            Err(_) => anyhow::bail!(
                "no space {id} here, where the move would write its file; a space on another machine moves from there, or with --ssh or --machine"
            ),
        }
    }

    /// Start a worker that finishes each recorded move whose worker went:
    /// one that holds no lock on it and that no operation runs.
    pub(crate) async fn finish_interrupted_moves(&self) -> Result<()> {
        for record in self.recorded_moves().await? {
            let space = record.space.clone();
            if self.operation_runs_in(&space) || MoveLock::try_take(&self.paths, &space)?.is_none()
            {
                continue;
            }
            log!("finishing the move of {space}, whose worker went");
            let spec = OperationSpec::FinishMove {
                record: Box::new(record),
            };
            self.spawn_operation(space, spec, None, None).await?;
        }
        Ok(())
    }

    /// Tell the user how a move whose worker went finished, as `ended` says.
    pub(crate) async fn tell_finished_move(&self, ended: &Ended) {
        let messages = match ended {
            Ended::Succeeded(outcome) => match outcome.as_ref() {
                Outcome::Moved(report) => finished_moves(report),
                _ => Vec::new(),
            },
            Ended::Failed(failure) => vec![format!(
                "A move that stopped when its worker went could not finish: {}",
                failure.message().unwrap_or("its worker failed")
            )],
        };
        for message in messages {
            self.notify(None, AlertKind::MoveFinished, message).await;
        }
    }

    /// Let a scoped `caller` move the spaces `order` takes, as its `admin`
    /// permission allows, refusing one its scope does not reach, or a move
    /// another machine sent, since a move reaches outside the hub's trust.
    pub(crate) async fn authorize_move(&self, caller: &Caller, order: &MoveOrder) -> Result<()> {
        ensure!(
            caller.vouched.is_none(),
            "only the user moves spaces from another machine"
        );
        let mut repository = None;
        for id in &order.spaces {
            let selector = SpaceSelector::id(id);
            match self.space(&selector).await {
                Ok(space) => {
                    self.ensure_reaches(caller, &space).await?;
                    repository = repository.or(space.repository_id);
                }
                Err(_) => self.ensure_restorable_by(&selector, caller).await?,
            }
        }
        self.administer(caller, order, repository).await
    }
}

/// What the user hears of the moves `report` finished after their worker
/// went: each space that ended here, and each that could not move.
fn finished_moves(report: &MoveReport) -> Vec<String> {
    let moved = (report.moved.iter()).map(|moved| {
        format!(
            "Finished moving {}, which stopped when its worker went; it ended here",
            moved.name
        )
    });
    let failed = (report.failed.iter()).map(|failed| {
        format!(
            "The move of {}, which stopped when its worker went, did not finish: {}",
            failed.name, failed.error
        )
    });
    moved.chain(failed).collect()
}
