//! Forwarding a thread: the current party of either side gives its role
//! to a space its scope reaches, or up to its coordinator or the user, with
//! a note, and the new party hears what the thread says so far. The
//! forwarder's space stays linked to it, hearing its later posts, unless it
//! unlinks. A new opener numbers the thread in its own space, and the
//! holder and the linkers hear its new number.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    model::Located,
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

/// Whom a forward gives the forwarder's role to: a space, none for the
/// user, and for the opener's role the space the thread is numbered in
/// from then on.
struct Successor {
    party: Option<ThreadSpace>,
    numbered_in: Option<SpaceId>,
}

/// A forward to record: who gives which side's role to whom, with what
/// note, whether the forwarder unlinks, and who opened the thread when the
/// forwarder's side was checked.
struct Forward {
    poster: Poster,
    side: Side,
    successor: Successor,
    note: String,
    unlink: bool,
    opener: (Sender, Option<SpaceId>),
}

impl Forward {
    /// Record the forward in the thread `id`, which must still be open with
    /// the forwarder's side as it was checked, give the role over, and tell
    /// whom it concerns; the thread as it is now.
    fn record(
        &self,
        tx: &Transaction<'_>,
        id: i64,
        outside: &dyn Records,
    ) -> Result<(Thread, Delivered)> {
        let before = records::reread(tx, id)?;
        ensure_open(&before)?;
        self.ensure_still_party(&before)?;
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
                forwards: Some(self.successor.party.as_ref()),
            },
        )?;
        self.pass_role(tx, &before)?;
        self.link_forwarder(tx, id)?;
        let thread = records::reread(tx, id)?;
        records::ensure_fits(&thread)?;
        let mut delivered = Delivered::default();
        self.tell(tx, outside, &before, &thread, &mut delivered)?;
        Ok((thread, delivered))
    }

    /// Refuse `thread` once its forwarder's side went to another party
    /// meanwhile.
    fn ensure_still_party(&self, thread: &Thread) -> Result<()> {
        match self.side {
            Side::Holder => records::ensure_held_by(thread, &self.poster.seen),
            Side::Opener => {
                ensure!(
                    (thread.opener, thread.opener_id().cloned()) == self.opener,
                    "{} opens thread {} now; act on it again if it is still yours",
                    thread.party(Side::Opener),
                    thread.number
                );
                Ok(())
            }
        }
    }

    /// Give the forwarder's role in the thread, as it was `before`, to the
    /// successor, numbering it in a new opener's space.
    fn pass_role(&self, tx: &Transaction<'_>, before: &Thread) -> Result<()> {
        let party = self.successor.party.as_ref();
        match self.side {
            Side::Holder => records::set_holder(tx, before.id, party),
            Side::Opener => {
                records::set_opener(tx, before.id, party)?;
                match &self.successor.numbered_in {
                    Some(space) if *space != before.space_id => {
                        records::renumber(tx, before.id, space).map(drop)
                    }
                    _ => Ok(()),
                }
            }
        }
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

    /// Tell the new party what `thread` says so far, and for a new opener
    /// the others who follow it where it went from `before`; nobody of a
    /// thread nobody heard of yet.
    fn tell(
        &self,
        tx: &Transaction<'_>,
        outside: &dyn Records,
        before: &Thread,
        thread: &Thread,
        delivered: &mut Delivered,
    ) -> Result<()> {
        if !thread.posts.iter().any(Post::speaks) {
            return Ok(());
        }
        let forward = (thread.posts.last()).context("the forward was not recorded")?;
        let mut reached: Vec<Party> = Party::sender(forward).into_iter().collect();
        if let Some(to) = Party::of(thread, self.side) {
            let texts = Texts::forwarded(thread, forward, self.side);
            delivery::deliver(tx, outside, thread, Some(forward), &to, texts, delivered)?;
            reached.push(to);
        }
        if self.side == Side::Opener {
            tell_reopened(tx, outside, before, thread, forward, &reached, delivered)?;
        }
        Ok(())
    }
}

/// Tell the holder of `thread` and the spaces that linked it, but those
/// `reached` already, that `forward` gave its opener's role away, which
/// made `before` what it is now.
fn tell_reopened(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    before: &Thread,
    thread: &Thread,
    forward: &Post,
    reached: &[Party],
    delivered: &mut Delivered,
) -> Result<()> {
    let text = reopened_text(before, thread, forward);
    let mut reached = reached.to_vec();
    let linkers = records::linkers(tx, thread.id)?
        .into_iter()
        .map(Party::Space);
    for party in Party::of(thread, Side::Holder).into_iter().chain(linkers) {
        if !reached.contains(&party) {
            let texts = Texts::notice(text.clone());
            delivery::deliver(tx, outside, thread, None, &party, texts, delivered)?;
            reached.push(party);
        }
    }
    Ok(())
}

/// What the holder and the linkers of `before` hear once `forward` gave its
/// opener's role away, which made it `after`.
fn reopened_text(before: &Thread, after: &Thread, forward: &Post) -> String {
    let mut text = format!("Thread {} ({})", before.number, before.one_line());
    if before.number != after.number {
        text.push_str(&format!(" is thread {} now,", after.number));
    }
    text.push_str(&format!(
        " opened by {}: {} passed it on.",
        after.party(Side::Opener),
        forward.author()
    ));
    text
}

impl Coordination {
    /// Give the role in the thread `number` of the side whose current party
    /// `caller` acts for to the space `to` names, else to that party's
    /// coordinator or the user, with `note` as a post; the new party
    /// receives the thread's posts from then on, and so does the caller's
    /// space unless `unlink`.
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
        let side = self.acting_side(daemon, caller, &thread).await?;
        ensure_open(&thread)?;
        let note = note.unwrap_or_default();
        if !note.is_empty() {
            crate::validate::agent_message(&note).context("invalid note")?;
        }
        let successor = match side {
            Side::Holder => self.next_holder(daemon, &thread, to).await?,
            Side::Opener => self.next_opener(daemon, &thread, to).await?,
        };
        let forward = Forward {
            poster: (self.poster(daemon, caller, Some(side), &thread.holder)).await?,
            side,
            successor,
            note,
            unlink,
            opener: (thread.opener, thread.opener_id().cloned()),
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
    ) -> Result<Successor> {
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
        Ok(Successor {
            party: holder,
            numbered_in: None,
        })
    }

    /// Who opens `thread` next: the space `to` names, else its opener's
    /// coordinator or the user; never its opener, nor its holder. The
    /// thread is numbered in the new opener's space, or the holder's for
    /// the user, which must be this machine's, since a thread does not move
    /// between machines.
    async fn next_opener(
        &self,
        daemon: &dyn Daemon,
        thread: &Thread,
        to: Option<&SpaceSelector>,
    ) -> Result<Successor> {
        let opener = match (to, thread.opener_id()) {
            (Some(to), _) => Some(here(daemon.space_anywhere(to).await?)?),
            (None, Some(opener)) => self.coordinator_of(daemon, opener).await?,
            (None, None) => bail!(
                "{} opened thread {}; name a space to forward it to",
                thread.party(Side::Opener),
                thread.number
            ),
        };
        let id = opener.as_ref().map(|opener| &opener.id);
        let same = match thread.opener {
            Sender::Space => id.is_some() && id == thread.opener_id(),
            Sender::User => id.is_none(),
            Sender::Shoal => false,
        };
        ensure!(
            !same,
            "{} opened thread {} already",
            thread.party(Side::Opener),
            thread.number
        );
        ensure!(
            id != thread.holder.space().map(|space| &space.id),
            "{} holds thread {}; it cannot open it too",
            thread.holder.label(),
            thread.number
        );
        let numbered_in = (opener.as_ref().or(thread.holder.space()))
            .map(|space| space.id.clone())
            .context("a thread the user opens is numbered in its holder's space")?;
        ensure!(
            daemon.find_space(&numbered_in).await?.is_some(),
            "thread {} would be numbered in a space on another machine, where Shoal moves no \
             thread yet",
            thread.number
        );
        Ok(Successor {
            party: opener,
            numbered_in: Some(numbered_in),
        })
    }
}

/// The space `located` as a thread names it, refusing one on another
/// machine, to which Shoal moves no thread yet.
fn here(located: Located) -> Result<ThreadSpace> {
    match located {
        Located::Here(space) => Ok(ThreadSpace::of(&space)),
        Located::Elsewhere { space, machine } => bail!(
            "{} runs on {}, where Shoal moves no thread yet",
            space.space.name,
            machine.name
        ),
    }
}
