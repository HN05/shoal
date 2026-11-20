//! Reading threads: listing and showing them, waiting for a reply,
//! linking one to receive its new posts, and which threads wait.
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Listing, Thread, ThreadNumber, ThreadState},
};

use super::records;
use crate::daemon::{item_numbers::ItemNumber, manager::Manager, scope::Caller};

/// Bytes of threads one listing returns, leaving room for the response
/// around them.
const LISTING_BUDGET: usize = crate::protocol::MAX_FRAME - 1024;
/// Rechecks a waited-for thread when no post wakes the wait, so removal of
/// its space ends it.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The newest of `threads` that fit one protocol response, oldest first.
fn newest_that_fit(mut threads: Vec<Thread>) -> Result<Listing> {
    let mut used = 0;
    let mut start = threads.len();
    for (index, thread) in threads.iter().enumerate().rev() {
        used += serde_json::to_vec(thread)?.len() + 1;
        if used > LISTING_BUDGET {
            break;
        }
        start = index;
    }
    let threads = threads.split_off(start);
    Ok(Listing {
        threads,
        omitted: start,
    })
}

impl Manager {
    /// The threads the space `selector` names takes part in, else every
    /// thread, in `state`, only those whose turn is the user's with
    /// `for_user`, the newest that fit one response, with their code
    /// followed to where it is now.
    pub async fn threads(
        &self,
        selector: Option<&SpaceSelector>,
        state: ThreadState,
        for_user: bool,
    ) -> Result<Listing> {
        let space = match selector {
            Some(selector) => Some(self.space(selector).await?.id),
            None => None,
        };
        let kept = match state {
            ThreadState::Open => records::OPEN,
            ThreadState::Resolved => "t.outcome IS NOT NULL",
            ThreadState::All => "1",
        };
        let mut threads = self
            .store
            .run(move |db| records::of_space(db, space.as_ref(), kept))
            .await?;
        self.follow_code(&mut threads).await;
        let threads = (threads.into_iter())
            .filter(|thread| !for_user || thread.waits_for_user())
            .collect();
        newest_that_fit(threads)
    }

    /// The thread `number` names, for `caller` to read, with its code
    /// followed to where it is now.
    pub async fn thread(&self, caller: Option<&Caller>, number: ThreadNumber) -> Result<Thread> {
        let mut thread = self.readable_thread(caller, number).await?;
        self.follow_code(std::slice::from_mut(&mut thread)).await;
        Ok(thread)
    }

    /// Every open thread with a post, oldest first, which subscription
    /// snapshots page and the hub lists. Neither follows code: both run
    /// on their own, a snapshot after each store change, where following
    /// would make a background pass over the worktrees.
    pub(crate) async fn open_threads(&self) -> Result<Vec<Thread>> {
        self.store.run(records::open_with_posts).await
    }

    /// Whether a thread the space `id` opened waits for its holder.
    pub async fn has_waiting_threads(&self, id: &SpaceId) -> Result<bool> {
        let id = id.clone();
        Ok(self.store.run(move |db| waiting_count(db, &id)).await? > 0)
    }

    /// Deliver the new posts of the thread `number` to the space
    /// `selector` names, which `caller` reads, or stop delivering them.
    pub async fn link_thread(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        number: ItemNumber,
        linked: bool,
    ) -> Result<Thread> {
        let space = self.space(selector).await?;
        let thread = (self.readable_thread(caller, ThreadNumber::Full(number))).await?;
        let id = thread.id;
        let changed = self
            .store
            .write(move |tx| records::set_link(tx, &space.id, id, linked))
            .await?;
        ensure!(
            changed,
            "thread {number} is {}linked already",
            if linked { "" } else { "not " }
        );
        Ok(thread)
    }

    /// The thread `item` of the space `selector` names, which `caller`
    /// reads, once the other side of the post `after` posts, or it is
    /// resolved. The space's ID finds
    /// it, since the hub may give the space another number meanwhile.
    pub async fn await_post(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        item: u32,
        after: i64,
    ) -> Result<Thread> {
        let space = self.space(selector).await?;
        let mut checked = false;
        // Subscribe before reading, so a post in between still wakes.
        let mut changed = self.signals.watch_threads();
        loop {
            let id = space.id.clone();
            let thread = self
                .store
                .run(move |db| records::by_item(db, &id, item))
                .await?
                .with_context(|| format!("{} has no thread {}.{item}", space.name, space.number))?;
            if !checked {
                ensure!(
                    self.reads(caller, &thread).await?,
                    "thread {} is not yours to read",
                    thread.number
                );
                checked = true;
            }
            let side = (thread.posts.iter())
                .find(|post| post.id == after)
                .map(|post| post.side)
                .with_context(|| format!("thread {} has no post {after}", thread.number))?;
            let answered = (thread.posts.iter())
                .any(|post| post.id > after && post.side != side && post.speaks());
            if answered || !thread.is_open() {
                return Ok(thread);
            }
            // A closed channel only disables its wake; the poll still rechecks.
            tokio::select! {
                Ok(()) = changed.changed() => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }
}

/// How many threads the space `id` opened wait for their holder.
pub(crate) fn waiting_count(db: &rusqlite::Connection, id: &SpaceId) -> Result<usize> {
    let opened = records::query(
        db,
        &format!(
            "WHERE t.space_id=?1 AND t.opener='space' AND {}",
            records::OPEN
        ),
        [id],
    )?;
    Ok(opened.iter().filter(|thread| thread.is_waiting()).count())
}

/// The newest open thread numbered in the space `id` whose turn is the
/// user's.
pub(crate) fn newest_waiting_for_user(
    db: &rusqlite::Connection,
    id: &SpaceId,
) -> Result<Option<Thread>> {
    let threads = records::query(
        db,
        &format!("WHERE t.space_id=?1 AND {}", records::OPEN),
        [id],
    )?;
    Ok(threads.into_iter().rev().find(Thread::waits_for_user))
}

/// Each space with a thread it opened that waits for a space that holds it,
/// with that holder.
pub(crate) fn waiting_on_spaces(db: &rusqlite::Connection) -> Result<Vec<(SpaceId, SpaceId)>> {
    let opened = records::query(
        db,
        &format!(
            "WHERE t.opener='space' AND t.holder_id IS NOT NULL AND {}",
            records::OPEN
        ),
        [],
    )?;
    Ok((opened.into_iter())
        .filter(Thread::is_waiting)
        .filter_map(|thread| Some((thread.space_id, thread.holder.space()?.id.clone())))
        .collect())
}
