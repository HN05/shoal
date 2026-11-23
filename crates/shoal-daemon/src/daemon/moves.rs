//! Moves the daemon runs as operations: started on the machine whose
//! spaces they take, by a worker that checks the spaces, packs them,
//! delivers the packages and ends the spaces here once their destination
//! has them, so a move goes on when the caller goes. A recorded move whose
//! worker went, as when the machine restarted during it, finishes in a
//! worker of its own, which tells the user how it ended.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use super::{
    hub_moves::moves_without_permission, log, manager::Manager, notifications::AlertKind,
    scope::Caller,
};
use crate::protocol::operations::{Ended, OperationSpec, Outcome};
use crate::transfer::{DeferredMove, MoveDestination, MoveLock, MoveOrder, MoveReport};
use shoal_protocol::{model::Located, space_names::SpaceSelector};

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
    /// when given, once every space it takes is here: live, or for a file
    /// or a joined machine a removed one. A space another machine has refuses it, naming that
    /// space, which the hub forwards it for, as does a joined machine to
    /// move to that is this one or offline. A move to a joined machine that
    /// would stop `caller`'s agent waits for its turn to end instead.
    pub(crate) async fn start_move(
        &self,
        order: MoveOrder,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<MoveStart> {
        ensure!(!order.spaces.is_empty(), "a move takes at least one space");
        for ordered in &order.spaces {
            self.ensure_movable(ordered, &order.destination).await?;
        }
        if let MoveDestination::Machine(destination) = &order.destination {
            self.receiving_machine(&destination.machine).await?;
            if let Some(caller) = caller
                && self.move_stops_caller(caller, &order).await?
            {
                return Ok(MoveStart::AfterTurn(
                    self.move_after_turn(caller, order).await?,
                ));
            }
        }
        let first = order.spaces[0].clone();
        let spec = OperationSpec::Move {
            order: Box::new(order),
        };
        let started = self.spawn_operation(first, spec, env, caller).await?;
        Ok(MoveStart::Started(started))
    }

    /// Refuse a space `id` that is not here to move to `destination`: a
    /// file or a joined machine takes a removed space too, and a file one
    /// another joined machine runs, whose package this machine's worker
    /// reads from there; a move to another machine, or one holding its
    /// package for this machine's, goes to the machine that has the space.
    async fn ensure_movable(&self, id: &SpaceId, destination: &MoveDestination) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let absent = match self.space(&selector).await {
            Ok(_) => return Ok(()),
            Err(absent) => absent,
        };
        if *destination == MoveDestination::Held {
            return Err(absent);
        }
        if self.removed_space(&selector).await.is_ok() {
            return Ok(());
        }
        if destination.machine().is_some() {
            return Err(absent);
        }
        match (self.space_anywhere(&selector).await?, destination) {
            (
                Located::Elsewhere { .. },
                MoveDestination::File { .. } | MoveDestination::Client { .. },
            ) => Ok(()),
            (Located::Elsewhere { machine, .. }, _) => anyhow::bail!(
                "space {id} runs on machine {}; move it to a file of its own, or from that machine",
                machine.name
            ),
            (Located::Here(_), _) => Err(absent),
        }
    }

    /// Start a worker that finishes each recorded move whose worker went:
    /// one that holds no lock on it and that no operation runs, nor one
    /// holding its package that the machine which started it may still
    /// read; link the
    /// spaces a finished move left here below the space they are below
    /// again, where its daemon stopped before it did; and settle the moves
    /// that waited for an agent's turn whose operation no longer runs.
    pub(crate) async fn finish_interrupted_moves(&self) -> Result<()> {
        self.attach_settled_behind().await?;
        self.forget_moves_cut_off().await?;
        let held = self.held_for_reader().await?;
        for record in self.recorded_moves().await? {
            let space = record.space.clone();
            if held.contains(&space)
                || self.operation_runs_in(&space)
                || MoveLock::try_take(&self.paths, &space)?.is_none()
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
            self.coordination
                .notify(self, None, AlertKind::MoveFinished, message)
                .await;
        }
    }

    /// Let a scoped `caller` move the spaces `order` takes, refusing one its
    /// scope does not reach: to a joined machine as
    /// [`Self::authorize_joined_move`] says, else as its `admin` permission
    /// allows, and never for a move another machine sent, since such a move
    /// reaches outside the hub's trust.
    pub(crate) async fn authorize_move(&self, caller: &Caller, order: &MoveOrder) -> Result<()> {
        if moves_without_permission(caller, order) {
            return self.authorize_joined_move(caller, &order.spaces).await;
        }
        ensure!(
            caller.vouched.is_none(),
            "only the user moves spaces from another machine"
        );
        let mut repository = None;
        for id in &order.spaces {
            let selector = SpaceSelector::id(id);
            match self.space(&selector).await {
                Ok(space) => {
                    self.coordination
                        .ensure_reaches(self, caller, &space)
                        .await?;
                    repository = repository.or(space.repository_id);
                }
                Err(_) => self.ensure_restorable_by(&selector, caller).await?,
            }
        }
        self.coordination
            .administer(self, caller, order, repository)
            .await
    }
}

/// How a move started.
pub(crate) enum MoveStart {
    /// In the operation of this ID.
    Started(String),
    /// Once the turn of the agent it would stop ends.
    AfterTurn(DeferredMove),
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
