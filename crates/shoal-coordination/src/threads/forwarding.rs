//! Forwarding a thread: its holder gives it to a space its scope reaches,
//! or up to its coordinator or the user, with a note, and the new holder
//! hears what it says so far. The forwarder's space stays linked to it,
//! hearing its later posts, unless it unlinks.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::Transaction;
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Holder, NewPost, Post, Sender, Side, Thread, ThreadNumber, ThreadSpace},
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    ensure_open,
    posting::Poster,
    records::{self, NewRecord},
};
use crate::{daemon::Records, scope::Caller};

/// A forward to record: who gives the thread to whom, none for the user,
/// with what note, and whether the forwarder unlinks.
struct Forward {
    poster: Poster,
    holder: Option<ThreadSpace>,
    note: String,
    unlink: bool,
}

impl Forward {
    /// Record the forward in the thread `id`, which must still be open and
    /// held as it was checked, give it to the new holder, and tell it; the
    /// thread as it is now.
    fn record(
        &self,
        tx: &Transaction<'_>,
        id: i64,
        outside: &dyn Records,
    ) -> Result<(Thread, Delivered)> {
        let before = records::reread(tx, id)?;
        ensure_open(&before)?;
        records::ensure_held_by(&before, &self.poster.seen)?;
        let post = NewPost {
            text: self.note.clone(),
            ..NewPost::default()
        };
        records::insert_post(
            tx,
            &NewRecord {
                thread: id,
                author: self.poster.author(),
                post: &post,
                files: &[],
                code: &[],
                in_chat: false,
                forwards: Some(self.holder.as_ref()),
            },
        )?;
        records::set_holder(tx, id, self.holder.as_ref())?;
        self.link_forwarder(tx, id)?;
        let thread = records::reread(tx, id)?;
        records::ensure_fits(&thread)?;
        let mut delivered = Delivered::default();
        tell(tx, outside, &thread, &mut delivered)?;
        Ok((thread, delivered))
    }

    /// Keep the thread `id` linked for the forwarder's space, or unlink it.
    fn link_forwarder(&self, tx: &Transaction<'_>, id: i64) -> Result<()> {
        let Some(forwarder) = &self.poster.space else {
            return Ok(());
        };
        if self.unlink {
            records::set_link(tx, &forwarder.id, id, false)?;
            Ok(())
        } else {
            records::link_here(tx, &forwarder.id, id)
        }
    }
}

/// Tell the new holder what `thread` says so far, unless nobody heard of
/// it yet.
fn tell(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    thread: &Thread,
    delivered: &mut Delivered,
) -> Result<()> {
    if !thread.posts.iter().any(Post::speaks) {
        return Ok(());
    }
    let forward = (thread.posts.last()).context("the forward was not recorded")?;
    let to = Party::of(thread, Side::Holder).context("a thread has a holder")?;
    let texts = Texts::forwarded(thread, forward);
    delivery::deliver(tx, outside, thread, Some(forward), &to, texts, delivered)?;
    Ok(())
}

impl Coordination {
    /// Give the thread `number`, which `caller` holds, to the space `to`
    /// names, else to its holder's coordinator or the user, with `note` as
    /// a post; the new holder receives the thread's posts from then on,
    /// and so does the caller's space unless `unlink`.
    pub async fn forward_thread(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        number: ThreadNumber,
        to: Option<&SpaceSelector>,
        note: Option<String>,
        unlink: bool,
    ) -> Result<Thread> {
        let thread = self.readable_thread(daemon, caller, number).await?;
        self.ensure_forwards(daemon, caller, &thread).await?;
        ensure_open(&thread)?;
        let note = note.unwrap_or_default();
        if !note.is_empty() {
            crate::validate::agent_message(&note).context("invalid note")?;
        }
        let forward = Forward {
            holder: self.next_holder(daemon, &thread, to).await?,
            poster: (self.poster(daemon, caller, Some(Side::Holder), &thread.holder)).await?,
            note,
            unlink,
        };
        let (id, outside) = (thread.id, self.records.clone());
        let (thread, delivered) = (self.store)
            .write(move |tx| forward.record(tx, id, &*outside))
            .await?;
        self.announce_threads(daemon, delivered).await;
        Ok(thread)
    }

    /// Whom `thread` goes to next: the space `to` names, here or on another
    /// machine, else its holder's coordinator or the user; never its
    /// holder, nor the space that opened it.
    async fn next_holder(
        &self,
        daemon: &dyn Daemon,
        thread: &Thread,
        to: Option<&SpaceSelector>,
    ) -> Result<Option<ThreadSpace>> {
        let holder = match (to, &thread.holder) {
            (Some(to), _) => Some(ThreadSpace::located(daemon.space_anywhere(to).await?)),
            (None, Holder::Space(holder)) => self.coordinator_of(daemon, &holder.id).await?,
            (None, Holder::User) => {
                bail!(
                    "the user holds thread {}; name a space to forward it to",
                    thread.number
                )
            }
        };
        let id = holder.as_ref().map(|holder| &holder.id);
        ensure!(
            id != thread.holder.space().map(|space| &space.id),
            "{} holds thread {} already",
            thread.holder.label(),
            thread.number
        );
        let opener = match thread.opener {
            Sender::Space => id.is_some() && id == thread.opener_id(),
            Sender::User => id.is_none(),
            Sender::Shoal => false,
        };
        ensure!(
            !opener,
            "{} opened thread {}; it cannot hold it too",
            thread.party(Side::Opener),
            thread.number
        );
        Ok(holder)
    }
}
