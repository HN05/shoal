//! Who takes part in a thread and what each may do: a caller posts from
//! the side it is on, the opener resolves, or the holder when Shoal opened
//! the thread, and the holder forwards. A space reads a thread it took part
//! in, or one a space its scope reaches took part in; the user reads every
//! thread and acts for either side.
use anyhow::{Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::{SpaceName, SpaceSelector},
    threads::{Holder, Sender, Side, Thread, ThreadNumber},
};

use super::records;
use crate::daemon::{item_numbers::ItemNumber, manager::Manager, scope::Caller};

impl Manager {
    /// The full number `number` names for `caller`: a bare number names a
    /// thread of the caller's own space, which the user has none of.
    pub(super) async fn thread_number(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
    ) -> Result<ItemNumber> {
        let own = match (number, caller) {
            (ThreadNumber::Own(_), Some(caller)) => self
                .find_space(&caller.space_id)
                .await?
                .map(|space| space.number),
            _ => None,
        };
        number.resolve(own)
    }

    /// The thread `number` names for `caller`, which it must be able to
    /// read.
    pub(super) async fn readable_thread(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
    ) -> Result<Thread> {
        let number = self.thread_number(caller, number).await?;
        let thread = self.store.run(move |db| records::known(db, number)).await?;
        ensure!(
            self.reads(caller, &thread).await?,
            "thread {number} is not yours to read: a space reads the threads it or a space below it took part in"
        );
        Ok(thread)
    }

    /// Whether `caller` reads `thread`: the user reads every thread, and a
    /// space those that a space its scope reaches took part in, as the
    /// space it is numbered in, a holder or an earlier holder.
    pub(super) async fn reads(&self, caller: Option<&Caller>, thread: &Thread) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(true);
        };
        let id = thread.id;
        let mut parties = self.store.run(move |db| records::holders(db, id)).await?;
        parties.push(thread.space_id.clone());
        for party in &parties {
            if self.reaches(caller, party).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The side of `thread` `caller` takes: the opener's for the space that
    /// opened it, the holder's for the space that holds it or a space above
    /// that one, and for the user the opener's side of a thread the user
    /// opened and else the holder's.
    pub(super) async fn side(&self, caller: Option<&Caller>, thread: &Thread) -> Result<Side> {
        let Some(caller) = caller else {
            return Ok(match thread.opener {
                Sender::User => Side::Opener,
                Sender::Space | Sender::Shoal => Side::Holder,
            });
        };
        if thread.opener == Sender::Space && caller.space_id == thread.space_id {
            return Ok(Side::Opener);
        }
        if let Holder::Space(holder) = &thread.holder
            && self.reaches(caller, &holder.id).await?
        {
            return Ok(Side::Holder);
        }
        bail!(
            "thread {} is between {} and {}; only they post in it",
            thread.number,
            thread.party(Side::Opener),
            thread.party(Side::Holder)
        )
    }

    /// The space `caller` speaks as, by its ID and name: one of this
    /// machine's, or one the hub lists on another machine, as a coordinator
    /// there replying to a thread of a space here.
    pub(super) async fn speaker(&self, caller: &Caller) -> Result<(SpaceId, SpaceName)> {
        if let Some(space) = self.find_space(&caller.space_id).await? {
            return Ok((space.id, space.name));
        }
        let selector = SpaceSelector::id(&caller.space_id);
        match self.hub.machine_of(self, &selector).await {
            Ok(Some((_, listed))) => Ok((listed.space.id, listed.space.name)),
            _ => bail!("no space {} here or on the hub", caller.space_id),
        }
    }

    /// Refuse `caller` resolving `thread` unless it is the opener's, or for
    /// a thread Shoal opened the holder's; the user resolves any thread.
    pub(super) async fn ensure_resolves(
        &self,
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<Side> {
        let side = self.side(caller, thread).await?;
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
        caller: Option<&Caller>,
        thread: &Thread,
    ) -> Result<()> {
        let side = self.side(caller, thread).await?;
        ensure!(
            side == Side::Holder,
            "{} holds thread {} and forwards it",
            thread.party(Side::Holder),
            thread.number
        );
        Ok(())
    }
}
