//! Threads: conversations between whoever opened one, the user, a space or
//! Shoal, and whoever holds it now, the user or a space. A space opens a
//! thread with its coordinator, else the user, or with a space below it;
//! the user opens one with a space. A thread is numbered in its opener's
//! space, or in the space the user opened it with, sharing that space's
//! numbers with approval requests, and keeps its number until that space
//! goes. Posts from either side reach the other as alerts, and the spaces
//! that linked the thread; the holder forwards it, and the opener resolves
//! it with a typed outcome.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Sender, Thread, ThreadKind},
};

use crate::daemon::{alerts::Recipients, manager::Manager, notifications::Source, scope::Caller};
use records::NewThread;

mod chat;
mod closing;
mod delivery;
mod forwarding;
mod moving;
mod parties;
mod posting;
mod reading;
mod records;
mod resolving;

pub(crate) use closing::{close, reopen};
pub(crate) use delivery::Delivered as ThreadsDelivered;
pub(crate) use reading::{newest_waiting_for_user, waiting_count, waiting_on_spaces};

impl Manager {
    /// Open an empty thread of `kind` named `name` from `caller`, or from
    /// the user without one, with the space `to` names, else with the
    /// caller's coordinator or the user. It notifies no one until its
    /// first post.
    pub async fn open_thread(
        &self,
        caller: Option<&Caller>,
        name: String,
        kind: ThreadKind,
        to: Option<&SpaceSelector>,
    ) -> Result<Thread> {
        crate::validate::message(&name).context("invalid thread name")?;
        let target = match to {
            Some(to) => Some(self.space(to).await?.id),
            None => None,
        };
        let (opener, space_id, holder) = match (caller, target) {
            (None, Some(target)) => (Sender::User, target.clone(), Some(target)),
            (None, None) => bail!("the user opens a thread with a space; name it with --space"),
            (Some(caller), Some(target)) => {
                ensure!(
                    target != caller.space_id,
                    "a space opens a thread with another space"
                );
                (Sender::Space, caller.space_id.clone(), Some(target))
            }
            (Some(caller), None) => {
                let holder = self.coordinator_of(&caller.space_id).await?;
                (Sender::Space, caller.space_id.clone(), holder)
            }
        };
        let removing = self.space_removals.clone();
        let thread = self
            .store
            .write(move |tx| {
                // A space reserved for removal takes no thread its cleanup
                // checks have not seen.
                removing.require_taking_work(tx, &space_id)?;
                let new = NewThread {
                    kind,
                    name: &name,
                    opener,
                    space_id: &space_id,
                    holder: holder.as_ref(),
                };
                let id = records::insert_thread(tx, &new)?;
                records::reread(tx, id)
            })
            .await?;
        self.signals.thread_changed();
        Ok(thread)
    }

    /// The space whose agent coordinates the space `id`, here or on another
    /// machine, or none for the user.
    async fn coordinator_of(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
        Ok(match self.route_up(id, Source::Agent).await? {
            Recipients::Coordinator(id) | Recipients::Above(id) => Some(id),
            Recipients::User | Recipients::Spaces(_) => None,
        })
    }
}

/// Refuse a resolved thread, naming how to reopen it.
fn ensure_open(thread: &Thread) -> Result<()> {
    match &thread.resolution {
        None => Ok(()),
        Some(resolution) => bail!(
            "thread {} is {}; reopen it with shoal thread resolve {} --undo",
            thread.number,
            resolution.outcome.describe(),
            thread.number
        ),
    }
}

#[cfg(test)]
mod tests;
