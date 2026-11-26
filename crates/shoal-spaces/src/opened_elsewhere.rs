//! Telling other machines what became of a space whose threads they keep,
//! such as its comments on their reviews. See `docs/design/reviews.md`.
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::Member,
    protocol::{Operation, ThreadMethod},
    threads::CloseCause,
};

use crate::{Spaces, daemon::Daemon, model::Space, threads};
use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub enum Followed {
    /// It finished or went: its threads there close.
    Closed(CloseCause),
    /// That was undone: the threads it closed reopen.
    Reopened(CloseCause),
}

impl Followed {
    fn operation(self, id: &SpaceId) -> Operation {
        let opener = id.clone();
        Operation::Thread(match self {
            Self::Closed(cause) => ThreadMethod::CloseOpened { opener, cause },
            Self::Reopened(cause) => ThreadMethod::ReopenOpened { opener, cause },
        })
    }
}

impl Spaces {
    /// The hub holds the request for a machine that is offline, and this
    /// machine's outbox while the hub cannot be reached. A machine's refusal
    /// is only logged, since what happened here stands.
    pub async fn tell_other_machines(&self, daemon: &dyn Daemon, id: &SpaceId, followed: Followed) {
        let machines = daemon.other_machines().await;
        let told = (machines.iter()).map(|machine| tell(daemon, machine, id, followed));
        join_all(told).await;
    }

    /// Skips a CI job's disposable workspace: it runs no agent, so it never
    /// wrote a review or opened a thread on another machine.
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

    /// Closes the threads the other machine's space opened here, and those
    /// it holds, telling their other openers. A removal also cancels its
    /// pending reviews here with their comments.
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

async fn tell(daemon: &dyn Daemon, machine: &Member, id: &SpaceId, followed: Followed) {
    let told = daemon.tell_machine(machine, followed.operation(id));
    if let Err(error) = told.await {
        crate::log!(
            "machine {} did not follow {followed:?} of {id}: {error:#}",
            machine.name
        );
    }
}
