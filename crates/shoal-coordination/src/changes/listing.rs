//! Reading changes. A caller reads them where it reads the space or the
//! thread.
use crate::{Coordination, daemon::Daemon};
use anyhow::{Context as _, Result};
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::changes::{Change, ChangeDiff};

use super::records;
use crate::{protocol::ChangeFilter, scope::Caller, threads::Followed, time::UnixTime};

/// Whose changes a reading returns.
enum Source {
    Space(SpaceId),
    Thread { id: i64, since: Option<UnixTime> },
}

impl Coordination {
    /// The changes `filter` keeps for `caller`, or the user without one,
    /// followed to where their code is now.
    pub async fn changes(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        filter: ChangeFilter,
    ) -> Result<Vec<Change>> {
        let source = self.change_source(daemon, caller, filter).await?;
        let read = (self.store).read(move |tx| source.read(tx, records::change));
        let mut changes = read.await?;
        let followed = changes.iter_mut().map(|change| change as &mut dyn Followed);
        self.follow(daemon, followed.collect()).await;
        Ok(changes)
    }

    /// The changes `filter` keeps for `caller`, with their lines up to a
    /// page's share, followed to where their code is now.
    pub async fn change_diffs(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        filter: ChangeFilter,
    ) -> Result<Vec<ChangeDiff>> {
        let source = self.change_source(daemon, caller, filter).await?;
        let read = (self.store).read(move |tx| source.read(tx, records::diff));
        let mut diffs: Vec<ChangeDiff> = (read.await?.into_iter())
            .map(ChangeDiff::cut_to_page)
            .collect();
        let followed = diffs
            .iter_mut()
            .map(|diff| &mut diff.change as &mut dyn Followed);
        self.follow(daemon, followed.collect()).await;
        Ok(diffs)
    }

    /// What `filter` reads for `caller`: the thread it names, which the
    /// caller must read, else the space, the caller's own by default.
    async fn change_source(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        filter: ChangeFilter,
    ) -> Result<Source> {
        let Some(number) = filter.thread else {
            let space = match (filter.space, caller) {
                (Some(selector), _) => daemon.space(&selector).await?.id,
                (None, Some(caller)) => caller.space_id.clone(),
                (None, None) => anyhow::bail!("name the space whose changes to read"),
            };
            return Ok(Source::Space(space));
        };
        let thread = self.readable_thread(daemon, caller, number).await?;
        let since = match filter.since {
            Some(post) => Some(
                (thread.posts.iter().filter(|post| post.speaks()))
                    .nth(post.saturating_sub(1) as usize)
                    .with_context(|| format!("thread {} has no post {post}", thread.number))?
                    .created_at,
            ),
            None => None,
        };
        Ok(Source::Thread {
            id: thread.id,
            since,
        })
    }
}

impl Source {
    fn read<T>(
        &self,
        db: &Connection,
        map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        match self {
            Self::Space(space) => records::latest(db, space, map),
            Self::Thread { id, since } => records::of_thread(db, *id, *since, map),
        }
    }
}
