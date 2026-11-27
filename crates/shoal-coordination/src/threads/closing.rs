//! Closing a space's threads when it finishes or goes, and reopening them
//! when that is undone.
use crate::daemon::Records;
use crate::{Coordination, daemon::Daemon};
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::threads::{
    CloseCause, NewPost, Outcome, OutcomeKind, Resolution, Sender, Side, Thread, ThreadSpace,
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    records::{self, Author, NewRecord},
};
use crate::item_numbers::ItemNumber;

/// Close the open threads the space `space_id` is numbered in, opened or
/// holds, for `cause`, telling their openers, but the space itself, that it
/// finished or went; their numbers and what that delivered.
pub fn close(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    space_id: &SpaceId,
    cause: CloseCause,
) -> Result<(Vec<ItemNumber>, Delivered)> {
    let open = records::query(
        tx,
        &format!(
            "WHERE {} AND (t.space_id=?1 OR t.opener_id=?1 OR t.holder_id=?1)
                AND t.space_number IS NOT NULL",
            records::OPEN
        ),
        [space_id],
    )?;
    let resolution = Resolution {
        outcome: Outcome::Closed { cause },
        by: Sender::Shoal,
        by_space: Some(space_id.clone()),
        resolved_at: crate::time::UnixTime::now(),
    };
    let mut delivered = Delivered::default();
    for thread in &open {
        records::set_resolution(tx, thread.id, Some(&resolution))?;
        let opener = Party::of(thread, Side::Opener);
        if opener.is_none() || opener == Some(Party::Space(space_id.clone())) {
            continue;
        }
        let label = party_label(tx, thread, space_id)?;
        let text = match cause {
            CloseCause::Completion => {
                format!("{label} finished its work, which closed the thread.")
            }
            CloseCause::Removal => format!("{label} was removed, which closed the thread."),
        };
        tell_opener(tx, outside, thread, text, &mut delivered)?;
    }
    Ok((open.iter().map(|thread| thread.number).collect(), delivered))
}

/// How Shoal's post about `thread` names the space `space_id` that closed
/// or reopened it: by its record here, else as the thread names it, as for
/// a space of another machine.
fn party_label(tx: &Transaction<'_>, thread: &Thread, space_id: &SpaceId) -> Result<String> {
    if let Some(label) = records::named(tx, space_id)? {
        return Ok(label);
    }
    let named = (thread.holder.space().into_iter())
        .chain(&thread.opener_space)
        .find(|space| space.id == *space_id);
    Ok(named.map_or_else(|| space_id.to_string(), ThreadSpace::label))
}

/// Record Shoal's post `text` on the holder's side of `thread` and
/// deliver it to the opener.
fn tell_opener(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    thread: &Thread,
    text: String,
    delivered: &mut Delivered,
) -> Result<()> {
    let post = NewPost {
        text,
        ..NewPost::default()
    };
    records::insert_post(
        tx,
        &NewRecord {
            thread: thread.id,
            author: Author {
                side: Some(Side::Holder),
                sender: Sender::Shoal,
                space: None,
            },
            post: &post,
            files: &[],
            code: &[],
            in_chat: false,
            forwards: None,
        },
    )?;
    let thread = records::reread(tx, thread.id)?;
    delivery::deliver_post(tx, outside, &thread, delivered)
}

/// Reopen the threads closed for `cause` by the space `space_id`, without
/// Shoal's posts that told their openers, and tell those openers; their
/// numbers and what that delivered. A thread a removal closed reopens once
/// every space it involves is back: the space it is numbered in, its
/// holder and the space whose removal closed it, whichever of them
/// `space_id` restores last.
pub fn reopen(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    space_id: &SpaceId,
    cause: CloseCause,
) -> Result<(Vec<ItemNumber>, Delivered)> {
    let closed = records::query(
        tx,
        &format!(
            "WHERE t.outcome=?3 AND t.cause=?2 AND t.space_number IS NOT NULL
                AND (t.resolver_id=?1 OR (?2='removal' AND (t.space_id=?1 OR t.holder_id=?1)))
                AND NOT {} AND NOT {}",
            removed("t.resolver_id"),
            removed("t.holder_id")
        ),
        params![space_id, cause, OutcomeKind::Closed],
    )?;
    let mut delivered = Delivered::default();
    for thread in &closed {
        let closed_at = (thread.resolution.as_ref()).map(|resolution| resolution.resolved_at);
        let told = records::drop_shoal_posts_since(tx, thread.id, closed_at)?;
        records::set_resolution(tx, thread.id, None)?;
        if told == 0 {
            continue;
        }
        let Some(opener) = Party::of(thread, Side::Opener) else {
            continue;
        };
        let reopened = records::reread(tx, thread.id)?;
        let label = party_label(tx, thread, space_id)?;
        let texts = Texts::resolved(&reopened, &label);
        delivery::deliver(tx, outside, &reopened, None, &opener, texts, &mut delivered)?;
    }
    Ok((
        closed.iter().map(|thread| thread.number).collect(),
        delivered,
    ))
}

impl Coordination {
    /// Reopen the threads closed for `cause` by the space `id`, as
    /// [`reopen`] does, telling their openers.
    pub async fn reopen_threads(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        cause: CloseCause,
    ) -> Result<()> {
        let id = id.clone();
        let outside = self.records.clone();
        let reopened = (self.store).write(move |tx| reopen(tx, &*outside, &id, cause));
        let (_, delivered) = reopened.await?;
        self.announce_threads(daemon, delivered).await;
        Ok(())
    }
}

/// SQL true when the space in `column` was removed and not restored: it
/// has a removal record and no live record. A space on another machine has
/// neither here.
fn removed(column: &str) -> String {
    format!(
        "({column} IS NOT NULL AND NOT EXISTS (SELECT 1 FROM spaces WHERE id={column})
            AND EXISTS (SELECT 1 FROM removed_spaces WHERE space_id={column}))"
    )
}
