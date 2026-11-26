//! Moves the daemon runs as operations, so a move goes on when its caller
//! goes. See `docs/design/moves.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use super::{hub_moves::moves_without_permission, log, notifications::AlertKind, scope::Caller};
use crate::protocol::operations::{Ended, OperationSpec, Outcome};
use crate::transfer::{DeferredMove, MoveDestination, MoveLock, MoveOrder, MoveReport};
use shoal_protocol::{hub::Member, model::Located, space_names::SpaceSelector};

const INTERRUPTED_MOVES_INTERVAL: Duration = Duration::from_secs(60);

/// Runs at startup and then every [`INTERRUPTED_MOVES_INTERVAL`].
pub async fn finish_interrupted(daemon: Arc<dyn Daemon>) {
    loop {
        {
            let _pass = daemon.spaces().gates.background_pass().await;
            if let Err(error) = daemon.spaces().finish_interrupted_moves(&*daemon).await {
                log!("finishing interrupted moves: {error:#}");
            }
        }
        tokio::time::sleep(INTERRUPTED_MOVES_INTERVAL).await;
    }
}

impl Spaces {
    /// The worker acts with `caller`'s scope. Every space the move takes must
    /// be here, live or removed: a space another machine has refuses it,
    /// naming that space, which the hub forwards it for. So does a joined
    /// destination that is this machine or offline. A move to a joined
    /// machine that would stop `caller`'s agent waits for its turn to end.
    pub async fn start_move(
        &self,
        daemon: &dyn Daemon,
        order: MoveOrder,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<MoveStart> {
        ensure!(!order.spaces.is_empty(), "a move takes at least one space");
        for ordered in &order.spaces {
            self.ensure_movable(daemon, ordered, &order.destination)
                .await?;
        }
        if let MoveDestination::Machine(destination) = &order.destination {
            self.receiving_machine(daemon, &destination.machine).await?;
            if let Some(caller) = caller
                && self.move_stops_caller(daemon, caller, &order).await?
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
        let started = self.operations.spawn(first, spec, env, caller).await?;
        Ok(MoveStart::Started(started))
    }

    /// A move to a file takes a space another joined machine runs or keeps
    /// the record of: this machine's worker reads its package from there. A
    /// move to another machine, or one holding its package for this
    /// machine's, goes to the machine that has the space or its record.
    async fn ensure_movable(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        destination: &MoveDestination,
    ) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let absent = match self.space(&selector).await {
            Ok(_) => return Ok(()),
            Err(absent) => absent,
        };
        if self.removed_space(&selector).await.is_ok() {
            return Ok(());
        }
        if *destination == MoveDestination::Held || destination.machine().is_some() {
            return Err(absent);
        }
        let Some(machine) = self.other_machine_with(daemon, &selector).await else {
            return Err(absent);
        };
        match destination {
            MoveDestination::File { .. }
            | MoveDestination::Directory { .. }
            | MoveDestination::Client { .. } => Ok(()),
            _ => anyhow::bail!(
                "space {id} is on machine {}; move it to a file, or from that machine",
                machine.name
            ),
        }
    }

    /// None when the hub lists neither or cannot be asked.
    async fn other_machine_with(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Option<Member> {
        if let Ok(Located::Elsewhere { machine, .. }) = self.space_anywhere(daemon, selector).await
        {
            return Some(*machine);
        }
        daemon.record_keeper(selector).await.ok().flatten()
    }

    /// Finishes each move whose worker went: one with no lock on it that no
    /// operation runs, and no package that the machine which started it may
    /// still read. Links the spaces a finished move left here below their
    /// parent again where the daemon stopped first. Settles the moves that
    /// waited for an agent's turn whose operation no longer runs, and
    /// deletes the copies' packages no machine came to read.
    pub async fn finish_interrupted_moves(&self, daemon: &dyn Daemon) -> Result<()> {
        self.attach_settled_behind().await?;
        self.forget_moves_cut_off(daemon).await?;
        if let Err(error) = self.forget_unread_copies().await {
            log!("deleting the copies no machine read: {error:#}");
        }
        let held = self.held_for_reader().await?;
        for record in self.recorded_moves().await? {
            let space = record.space.clone();
            if held.contains(&space)
                || self.operations.runs_in(&space)
                || MoveLock::try_take(&self.paths, &space)?.is_none()
            {
                continue;
            }
            log!("finishing the move of {space}, whose worker went");
            let spec = OperationSpec::FinishMove {
                record: Box::new(record),
            };
            self.operations.spawn(space, spec, None, None).await?;
        }
        Ok(())
    }

    pub async fn tell_finished_move(&self, daemon: &dyn Daemon, ended: &Ended) {
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
            daemon
                .coordination()
                .notify(daemon, None, AlertKind::MoveFinished, message)
                .await;
        }
    }

    /// Refuses a space the caller's scope does not reach. A move to a joined
    /// machine follows [`Self::authorize_joined_move`], any other the
    /// `admin` permission. A move another machine sent only holds the
    /// packages for that machine, which writes them where its caller named:
    /// a file, or a host `ssh` reaches, here lies outside the hub's trust.
    pub async fn authorize_move(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        order: &MoveOrder,
    ) -> Result<()> {
        if moves_without_permission(caller, order) {
            return self
                .authorize_joined_move(daemon, caller, &order.spaces)
                .await;
        }
        ensure!(
            caller.vouched.is_none() || order.destination == MoveDestination::Held,
            "a move another machine sends only holds its packages for that machine"
        );
        let mut repository = None;
        for id in &order.spaces {
            let selector = SpaceSelector::id(id);
            match self.space(&selector).await {
                Ok(space) => {
                    daemon
                        .coordination()
                        .ensure_reaches(daemon, caller, &space)
                        .await?;
                    repository = repository.or(space.repository_id);
                }
                Err(_) => self.ensure_restorable_by(daemon, &selector, caller).await?,
            }
        }
        daemon
            .coordination()
            .administer(daemon, caller, order, repository)
            .await
    }
}

pub enum MoveStart {
    Started(String),
    /// Once the turn of the agent the move would stop ends.
    AfterTurn(DeferredMove),
}

/// Each space that ended here, and each that could not move.
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
