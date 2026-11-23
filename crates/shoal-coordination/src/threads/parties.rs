//! Who takes part in a thread and what each may do: a caller posts from
//! the side it took part on, or on neither as a participant that reads the
//! thread; the opener resolves, or the holder when Shoal opened the thread,
//! and the holder forwards. A space reads a thread it took part in, or one
//! a space its scope reaches took part in; the user reads every thread and
//! acts for either side.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Holder, Sender, Side, Thread, ThreadNumber, ThreadSpace},
};

use super::records;
use crate::{item_numbers::ItemNumber, scope::Caller};

impl Coordination {
    /// The full number `number` names for `caller`: a bare number names a
    /// thread of the caller's own space, which the user has none of.
    pub(super) async fn thread_number(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        number: ThreadNumber,
    ) -> Result<ItemNumber> {
        let own = match (number, caller) {
            (ThreadNumber::Own(_), Some(caller)) => daemon
                .find_space(&caller.space_id)
                .await?
                .map(|space| space.number),
            _ => None,
        };
        number.resolve(own)
    }

    /// The thread `number` names for `caller`, which it must be able to
    /// read.
    pub async fn readable_thread(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        number: ThreadNumber,
    ) -> Result<Thread> {
        let number = self.thread_number(daemon, caller, number).await?;
        let thread = self.store.run(move |db| records::known(db, number)).await?;
        ensure!(
            self.reads(daemon, caller, &thread).await?,
            "thread {number} is not yours to read: a space reads the threads it or a space below it took part in"
        );
        Ok(thread)
    }

    /// Whether `caller` reads `thread`: the user reads every thread, and a
    /// space those that a space its scope reaches took part in, as the
    /// space it is numbered in, its opener, a holder or an earlier holder,
    /// except a pending review's comment, which only its writer's side
    /// reads.
    pub(super) async fn reads(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(true);
        };
        if thread.held {
            return self.reads_held(daemon, Some(caller), thread).await;
        }
        let id = thread.id;
        let mut parties = self.store.run(move |db| records::holders(db, id)).await?;
        parties.push(thread.space_id.clone());
        parties.extend(thread.opener_id().cloned());
        for party in &parties {
            if self.reaches(daemon, caller, party).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `caller` reads a thread a post of which sent the attached
    /// file `file`, whose copy it then reads too, in whichever space it is
    /// kept.
    pub async fn reads_posted_file(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        file: i64,
    ) -> Result<bool> {
        let threads = (self.store)
            .read(move |db| records::posting_file(db, file))
            .await?;
        for thread in &threads {
            if self.reads(daemon, Some(caller), thread).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `caller` reads `thread` as far as its posts are held: any
    /// thread that holds none, and a pending review's comment for the user
    /// and for a space that reaches the space that writes it.
    pub async fn reads_held(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<bool> {
        match (caller, thread.held, thread.opener_id()) {
            (None, _, _) | (_, false, _) => Ok(true),
            (Some(caller), true, Some(writer)) => self.reaches(daemon, caller, writer).await,
            (Some(_), true, None) => Ok(false),
        }
    }

    /// The side of `thread` whose current party `caller` acts for: the
    /// opener's for the space that opened it, the holder's for the space
    /// that holds it or a space above that one, and for the user the
    /// opener's side of a thread the user opened and else the holder's;
    /// none for any other space. A holder takes no side of a thread that
    /// holds its posts, which it has not heard of yet.
    async fn current_side(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<Option<Side>> {
        let Some(caller) = caller else {
            return match thread.opener {
                Sender::User => Ok(Some(Side::Opener)),
                Sender::Space | Sender::Shoal => {
                    ensure_reached(thread)?;
                    Ok(Some(Side::Holder))
                }
            };
        };
        if thread.opener_id() == Some(&caller.space_id) {
            return Ok(Some(Side::Opener));
        }
        if let Holder::Space(holder) = &thread.holder
            && self.reaches(daemon, caller, &holder.id).await?
        {
            ensure_reached(thread)?;
            return Ok(Some(Side::Holder));
        }
        Ok(None)
    }

    /// The side `caller` posts on in `thread`, which it reads: the side
    /// whose current party it acts for, else the side it took part on
    /// before, else none, as a participant. Nobody but the opener's side
    /// posts in a thread that holds its posts.
    pub(super) async fn posting_side(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<Option<Side>> {
        let current = self.current_side(daemon, caller, thread).await?;
        let side = match (current, caller) {
            (Some(_), _) | (None, None) => current,
            (None, Some(caller)) => {
                let (id, space) = (thread.id, caller.space_id.clone());
                (self.store)
                    .read(move |db| records::earlier_side(db, id, &space))
                    .await?
            }
        };
        if side != Some(Side::Opener) {
            ensure_reached(thread)?;
        }
        Ok(side)
    }

    /// The side of `thread` whose current party `caller` acts for, which
    /// forwards that side's role and resolves; refused for any other
    /// caller.
    async fn acting_side(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<Side> {
        (self.current_side(daemon, caller, thread).await?).with_context(|| {
            format!(
                "thread {} is between {} and {}; only they forward or resolve it",
                thread.number,
                thread.party(Side::Opener),
                thread.party(Side::Holder)
            )
        })
    }

    /// The space `caller` speaks as: one of this machine's, or one the hub
    /// lists on another machine, as a coordinator there replying to a
    /// thread of a space here.
    pub(super) async fn speaker(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
    ) -> Result<ThreadSpace> {
        let selector = SpaceSelector::id(&caller.space_id);
        Ok(ThreadSpace::located(
            daemon.space_anywhere(&selector).await?,
        ))
    }

    /// The space `id`, here or on another machine, by the number and name
    /// it has there, or by its ID alone while the hub cannot be asked.
    pub(super) async fn named_space(&self, daemon: &dyn Daemon, id: SpaceId) -> ThreadSpace {
        match daemon.space_anywhere(&SpaceSelector::id(&id)).await {
            Ok(located) => ThreadSpace::located(located),
            Err(error) => {
                crate::log!("space {id} is named by its ID: {error:#}");
                ThreadSpace::here(id)
            }
        }
    }

    /// Refuse `caller` resolving `thread` unless it is the opener's, or for
    /// a thread Shoal opened the holder's; the user resolves any thread.
    pub(super) async fn ensure_resolves(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<Side> {
        let side = self.acting_side(daemon, caller, thread).await?;
        let resolver = match thread.opener {
            Sender::Shoal => Side::Holder,
            Sender::User | Sender::Space => Side::Opener,
        };
        ensure!(
            caller.is_none() || side == resolver,
            "{} resolves thread {}",
            thread.party(resolver),
            thread.number
        );
        Ok(side)
    }

    /// Refuse `caller` forwarding `thread` unless it is on the holder's
    /// side.
    pub(super) async fn ensure_forwards(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<()> {
        let side = self.acting_side(daemon, caller, thread).await?;
        ensure!(
            side == Side::Holder,
            "{} holds thread {} and forwards it",
            thread.party(Side::Holder),
            thread.number
        );
        Ok(())
    }
}

/// Refuse the holder's side of `thread` while it holds its posts, as a
/// pending review's comment does until the review is submitted.
fn ensure_reached(thread: &Thread) -> Result<()> {
    ensure!(
        !thread.held,
        "thread {} is a comment of a pending review, which reaches its holder once the review \
         is submitted",
        thread.number
    );
    Ok(())
}
