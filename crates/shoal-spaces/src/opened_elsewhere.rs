//! The threads a space opened that other machines keep: its comments of
//! the reviews they keep of their workspaces. When the space finishes,
//! goes or comes back, each other machine closes or reopens them, and the
//! threads there it holds, as its own machine does those it keeps, and its
//! removal cancels its pending reviews there; the hub holds the request
//! for a machine that is offline.
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{Member, routing::Queued},
    protocol::{Operation, ThreadMethod},
    threads::CloseCause,
};

use crate::{Spaces, daemon::Daemon, threads};
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
    /// of the space `id`, or the hub hold it for one that is offline. A
    /// machine that cannot be asked is logged, since what happened here
    /// stands.
    pub async fn tell_other_machines(&self, daemon: &dyn Daemon, id: &SpaceId, followed: Followed) {
        let machines = daemon.other_machines().await;
        let told = (machines.iter()).map(|machine| tell(daemon, machine, id, followed));
        join_all(told).await;
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
/// logging a failure other than the hub holding the request.
async fn tell(daemon: &dyn Daemon, machine: &Member, id: &SpaceId, followed: Followed) {
    let asked = daemon.ask_machine_or_hold(machine, followed.operation(id), None);
    match asked.await {
        Ok(_) => {}
        Err(error) if error.is::<Queued>() => {}
        Err(error) => crate::log!(
            "machine {} did not follow {followed:?} of {id}: {error:#}",
            machine.name
        ),
    }
}
