//! Closing a space's threads when it finishes or goes, and reopening them
//! when that is undone: `done` and removal close every open thread the
//! space is numbered in or holds, and tell the opener of one it holds that
//! the holder finished; `done --undo` and a restore reopen the threads
//! closed for that cause that nothing resolved otherwise since.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::threads::{CloseCause, NewPost, Outcome, Resolution, Sender, Side, Thread};

use super::{
    delivery::{self, Delivered, Party, Texts},
    records::{self, Author, NewRecord},
};
use crate::daemon::{item_numbers::ItemNumber, manager::Manager};

/// Close the open threads the space `space_id` is numbered in or holds,
/// for `cause`, telling their openers, but the space itself, that it
/// finished or went; their numbers and what that delivered.
pub(crate) fn close(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    cause: CloseCause,
) -> Result<(Vec<ItemNumber>, Delivered)> {
    let open = records::query(
        tx,
        &format!(
            "WHERE {} AND (t.space_id=?1 OR t.holder_id=?1) AND t.space_number IS NOT NULL",
            records::OPEN
        ),
        [space_id],
    )?;
    let label = records::label(tx, space_id)?;
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
        let text = match cause {
            CloseCause::Completion => {
                format!("{label} finished its work, which closed the thread.")
            }
            CloseCause::Removal => format!("{label} was removed, which closed the thread."),
        };
        tell_opener(tx, thread, text, &mut delivered)?;
    }
    Ok((open.iter().map(|thread| thread.number).collect(), delivered))
}

/// Record Shoal's post `text` on the holder's side of `thread` and
/// deliver it to the opener.
fn tell_opener(
    tx: &Transaction<'_>,
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
                side: Side::Holder,
                sender: Sender::Shoal,
                space: None,
            },
            post: &post,
            files: &[],
            in_chat: false,
            forwards: None,
        },
    )?;
    let thread = records::reread(tx, thread.id)?;
    delivery::deliver_post(tx, &thread, delivered)
}

/// Reopen the threads closed for `cause` by the space `space_id`, without
/// Shoal's posts that told their openers, and tell those openers; their
/// numbers and what that delivered.
pub(crate) fn reopen(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    cause: CloseCause,
) -> Result<(Vec<ItemNumber>, Delivered)> {
    let closed = records::query(
        tx,
        "WHERE t.resolver_id=?1 AND json_extract(t.outcome,'$.outcome')='closed'
            AND json_extract(t.outcome,'$.cause')=?2 AND t.space_number IS NOT NULL",
        params![space_id, cause],
    )?;
    let label = records::label(tx, space_id)?;
    let mut delivered = Delivered::default();
    for thread in &closed {
        let told = tx.execute(
            "DELETE FROM thread_posts WHERE thread_id=?1 AND sender='shoal' AND created_at>=?2",
            params![
                thread.id,
                thread
                    .resolution
                    .as_ref()
                    .map(|resolution| resolution.resolved_at)
            ],
        )?;
        records::set_resolution(tx, thread.id, None)?;
        if told == 0 {
            continue;
        }
        let Some(opener) = Party::of(thread, Side::Opener) else {
            continue;
        };
        let reopened = records::reread(tx, thread.id)?;
        let texts = Texts::resolved(&reopened, &label);
        delivery::deliver(tx, &reopened, None, &opener, texts, &mut delivered)?;
    }
    Ok((
        closed.iter().map(|thread| thread.number).collect(),
        delivered,
    ))
}

impl Manager {
    /// Reopen the threads closed for `cause` by the space `id`, as
    /// [`reopen`] does, telling their openers.
    pub(crate) async fn reopen_threads(&self, id: &SpaceId, cause: CloseCause) -> Result<()> {
        let id = id.clone();
        let (_, delivered) = (self.store).write(move |tx| reopen(tx, &id, cause)).await?;
        self.announce_threads(delivered).await;
        Ok(())
    }
}
