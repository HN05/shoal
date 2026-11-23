//! The threads a space opened that other machines keep: its comments of
//! the reviews they keep of their workspaces. When the space finishes,
//! goes or comes back, each other machine closes or reopens them, and the
//! threads there it holds, as its own machine does those it keeps, and its
//! removal cancels its pending reviews there; the hub holds the request
//! for a machine that is offline, and this machine's outbox while the hub
//! cannot be reached.
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::Member,
    protocol::{Operation, ThreadMethod},
    threads::CloseCause,
};

use crate::{Spaces, daemon::Daemon, model::Space, threads};
use anyhow::Result;

/// What became of a space, which the other machines follow with the
/// threads they keep of it.
#[derive(Debug, Clone, Copy)]
pub enum Followed {
    /// It finished or went: its threads there close.
    Closed(CloseCause),
    /// That was undone: the threads it closed reopen.
    Reopened(CloseCause),
}

impl Followed {
    /// The request that has another machine follow it for the space `id`.
    fn operation(self, id: &SpaceId) -> Operation {
        let opener = id.clone();
        Operation::Thread(match self {
            Self::Closed(cause) => ThreadMethod::CloseOpened { opener, cause },
            Self::Reopened(cause) => ThreadMethod::ReopenOpened { opener, cause },
        })
    }
}

impl Spaces {
    /// Have every other joined machine follow what `followed` says became
    /// of the space `id`, once it and the hub can be reached. A machine
    /// that refuses is logged, since what happened here stands.
    pub async fn tell_other_machines(&self, daemon: &dyn Daemon, id: &SpaceId, followed: Followed) {
        let machines = daemon.other_machines().await;
        let told = (machines.iter()).map(|machine| tell(daemon, machine, id, followed));
        join_all(told).await;
    }

    /// Have every other joined machine close the threads the removed
    /// `space` opened or holds there and cancel its pending reviews there,
    /// unless it is a CI job's disposable workspace, which runs no agent, so
    /// it never wrote a review or opened a thread on another machine.
    pub async fn tell_removed(&self, daemon: &dyn Daemon, space: &Space) {
        if space
            .to_workspace()
            .is_some_and(|workspace| workspace.disposable)
        {
            return;
        }
        let closed = Followed::Closed(CloseCause::Removal);
        self.tell_other_machines(daemon, &space.id, closed).await;
    }

    /// Close the threads here that the space `opener` of another machine
    /// opened, and those it holds, for `cause`, as its machine closes its
    /// own, telling their other openers; a removal also cancels its pending
    /// reviews here with their comments.
    pub async fn close_opened(
        &self,
        daemon: &dyn Daemon,
        opener: &SpaceId,
        cause: CloseCause,
    ) -> Result<()> {
        let (id, records) = (opener.clone(), self.records.clone());
        let (_, delivered) = (self.store)
            .write(move |tx| {
                if cause == CloseCause::Removal {
                    records.drop_pending_reviews(tx, &id)?;
                }
                threads::close(tx, records.coordination(), &id, cause)
            })
            .await?;
        let coordination = daemon.coordination();
        coordination.announce_threads(daemon, delivered).await;
        Ok(())
    }
}

/// Have `machine` follow what `followed` says became of the space `id`,
/// logging its refusal.
async fn tell(daemon: &dyn Daemon, machine: &Member, id: &SpaceId, followed: Followed) {
    let told = daemon.tell_machine(machine, followed.operation(id));
    if let Err(error) = told.await {
        crate::log!(
            "machine {} did not follow {followed:?} of {id}: {error:#}",
            machine.name
        );
    }
}
