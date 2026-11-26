//! Handing a space's open threads to the space that absorbs its work.
use crate::daemon::Records;
use crate::{Coordination, daemon::Daemon};
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::threads::{Side, Thread, ThreadSpace};

use super::{
    delivery::{self, Delivered, Party, Texts},
    records,
};
use crate::item_numbers::ItemNumber;

/// Give the open threads of the space `from` to the space `to`, as the
/// module says, pointing the posts `from` sent at the copies of their
/// files, which `copies` maps each file's record ID to; the moved threads'
/// numbers, as they are now, and what telling their other side delivered.
fn move_open(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    from: &SpaceId,
    to: &SpaceId,
    copies: &HashMap<i64, i64>,
) -> Result<(Vec<ItemNumber>, Delivered)> {
    let open = records::query(
        tx,
        &format!(
            "WHERE {} AND t.space_number IS NOT NULL AND (t.space_id=?1 OR t.holder_id=?1)
                AND t.space_id<>?2 AND t.holder_id IS NOT ?2",
            records::OPEN
        ),
        params![from, to],
    )?;
    let (absorber, absorbed) = (records::label(tx, to)?, records::label(tx, from)?);
    let mut moved = Vec::new();
    let mut delivered = Delivered::default();
    for before in open {
        let numbered_here = &before.space_id == from;
        if numbered_here {
            records::renumber(tx, before.id, to)?;
        }
        if before
            .holder
            .space()
            .is_some_and(|holder| &holder.id == from)
        {
            records::set_holder(tx, before.id, Some(&ThreadSpace::here(to.clone())))?;
        }
        repoint_files(tx, before.id, from, to, copies)?;
        let after = records::reread(tx, before.id)?;
        // The side that stays hears where the thread went.
        let stays = if numbered_here {
            Side::Holder
        } else {
            Side::Opener
        };
        if let Some(party) = Party::of(&after, stays)
            && after.posts.iter().any(|post| post.speaks())
        {
            let texts = Texts::notice(moved_text(&before, &after, &absorber, &absorbed));
            delivery::deliver(tx, outside, &after, None, &party, texts, &mut delivered)?;
        }
        moved.push(after.number);
    }
    Ok((moved, delivered))
}

/// The posts `from` sent in the thread `id` name the copies of their files,
/// and `to` as their sender.
fn repoint_files(
    tx: &Transaction<'_>,
    id: i64,
    from: &SpaceId,
    to: &SpaceId,
    copies: &HashMap<i64, i64>,
) -> Result<()> {
    let sent: Vec<(i64, String)> = tx
        .prepare("SELECT id,files FROM thread_posts WHERE thread_id=?1 AND sender_id=?2")?
        .query_map(params![id, from], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (post, files) in sent {
        let files: Vec<i64> = serde_json::from_str(&files)?;
        let files: Vec<i64> = (files.iter())
            .filter_map(|file| copies.get(file).copied())
            .collect();
        tx.execute(
            "UPDATE thread_posts SET files=?2,sender_id=?3 WHERE id=?1",
            params![post, serde_json::to_string(&files)?, to],
        )?;
    }
    Ok(())
}

/// What the side that stays is told of `before`, which is `after` now that
/// `absorber` took over the work of `absorbed`.
fn moved_text(before: &Thread, after: &Thread, absorber: &str, absorbed: &str) -> String {
    let mut text = format!("Thread {} ({})", before.number, before.one_line());
    if before.number != after.number {
        text.push_str(&format!(" is thread {} now", after.number));
    }
    if before.holder != after.holder {
        let joiner = if before.number == after.number {
            ""
        } else {
            ","
        };
        text.push_str(&format!("{joiner} held by {}", after.holder.label()));
    }
    text.push_str(&format!(": {absorber} took over the work of {absorbed}."));
    text
}

impl Coordination {
    /// Give the open threads of the space `from` to the space `to`, as
    /// [`move_open`] does, and return their numbers as they are now.
    pub async fn move_open_threads(
        &self,
        daemon: &dyn Daemon,
        from: &SpaceId,
        to: &SpaceId,
        copies: HashMap<i64, i64>,
    ) -> Result<Vec<ItemNumber>> {
        let (from, to, outside) = (from.clone(), to.clone(), self.records.clone());
        let (moved, delivered) = (self.store)
            .write(move |tx| move_open(tx, &*outside, &from, &to, &copies))
            .await?;
        self.announce_threads(daemon, delivered).await;
        Ok(moved)
    }
}
