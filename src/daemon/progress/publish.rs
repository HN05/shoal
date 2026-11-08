//! Showing each space's progress in the progress section of its open PRs,
//! those it opened or was assigned and has not marked ready: at most one
//! edit per space each [`INTERVAL`], the latest progress written in the
//! end. A failed edit is logged and retried with the next change, without
//! a notification.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::Result;
use rusqlite::Connection;
use tokio::{sync::Notify, time::Instant};

use super::{mark_published, section, stored};
use crate::{
    client::space_names::SpaceSelector,
    daemon::{
        links::{space::LinkSpace, wait::linked_items},
        log, store,
        workspace::Manager,
    },
    forge::items::link::ItemKind,
    model::Relation,
};

/// The shortest time between two publications of one space's progress.
const INTERVAL: Duration = Duration::from_secs(60);

/// The spaces whose progress changed since the publisher last looked.
#[derive(Default)]
pub(crate) struct Publications {
    requested: Mutex<HashSet<String>>,
    wake: Notify,
}

impl Publications {
    /// Publish the progress of the space `id` once its interval allows.
    pub(crate) fn request(&self, id: &str) {
        self.lock().insert(id.to_owned());
        self.wake.notify_one();
    }

    fn take(&self) -> Vec<String> {
        self.lock().drain().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// When each requested space may publish, one [`INTERVAL`] after it last
/// did.
#[derive(Default)]
struct Schedule {
    due: HashMap<String, Instant>,
    published: HashMap<String, Instant>,
}

impl Schedule {
    /// Publish the space `id` as soon as its interval allows after `now`; a
    /// space already due keeps its time, and publishes what it has then.
    fn request(&mut self, id: String, now: Instant) {
        let earliest =
            (self.published.get(&id)).map_or(now, |published| now.max(*published + INTERVAL));
        self.due.entry(id).or_insert(earliest);
    }

    /// When the next space is due.
    fn next(&self) -> Option<Instant> {
        self.due.values().min().copied()
    }

    /// The spaces due at `now`, which publish then.
    fn take_due(&mut self, now: Instant) -> Vec<String> {
        let due: Vec<String> = (self.due.iter())
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &due {
            self.due.remove(id);
            self.published.insert(id.clone(), now);
        }
        self.published
            .retain(|_, published| now < *published + INTERVAL);
        due
    }
}

/// Publish what changed while the daemon was down, then each change as its
/// space's interval allows.
pub async fn run(manager: Arc<Manager>) {
    match manager.unpublished_progress().await {
        Ok(spaces) => spaces
            .iter()
            .for_each(|id| manager.publications.request(id)),
        Err(error) => log!("progress sections: {error:#}"),
    }
    let mut schedule = Schedule::default();
    loop {
        let now = Instant::now();
        for id in manager.publications.take() {
            schedule.request(id, now);
        }
        for id in schedule.take_due(now) {
            if let Err(error) = manager.publish_progress(&id).await {
                log!("progress sections of space {id}: {error:#}");
            }
        }
        let next = schedule.next();
        tokio::select! {
            _ = manager.publications.wake.notified() => {}
            _ = tokio::time::sleep_until(next.unwrap_or(now)), if next.is_some() => {}
        }
    }
}

/// The open PRs the space `id` opened or was assigned and has not marked
/// ready, which show its progress.
fn publishing_pulls(db: &Connection, id: &str) -> Result<Vec<String>> {
    let assigned = linked_items(db, id, Some(ItemKind::Pr))?
        .into_iter()
        .map(|(url, _)| url);
    let opened = (store::recorded_items(db, id, Relation::Opened)?.into_iter())
        .filter(|item| item.kind == ItemKind::Pr.into())
        .map(|item| item.url);
    let marked = crate::daemon::workspace::review::list(db, id)?;
    let mut urls: Vec<String> = Vec::new();
    for url in assigned.chain(opened) {
        let ready = marked.iter().any(|mark| mark.url.as_ref() == Some(&url));
        if !ready && !urls.contains(&url) {
            urls.push(url);
        }
    }
    Ok(urls)
}

impl Manager {
    /// Write the progress of the space `id` into the progress section of
    /// each PR that shows it, unless `[pr] progress` is off, and record it
    /// as published once every one does.
    pub(crate) async fn publish_progress(&self, id: &str) -> Result<()> {
        let space = self.link_space(&SpaceSelector::id(id)).await?;
        if !self.space_settings(&space).await?.pr.progress {
            return Ok(());
        }
        let _gates = self.lock_change_gates(id, &[]).await?;
        let query = id.to_owned();
        let (stored, urls) = (self.store)
            .read(move |db| Ok((stored(db, &query)?, publishing_pulls(db, &query)?)))
            .await?;
        let Some((plan, updated_at)) = stored else {
            return Ok(());
        };
        let section = (!plan.is_empty()).then(|| section::render(&plan.progress(updated_at)));
        if !self.write_sections(&space, &urls, section.as_deref()).await {
            return Ok(());
        }
        let query = id.to_owned();
        (self.store)
            .write(move |db| mark_published(db, &query, &plan))
            .await
    }

    /// Show `section` in the space's PRs at `urls`, or remove it for
    /// `None`, logging each PR that fails without stopping at it; whether
    /// every one succeeded. The caller holds the gates
    /// [`Manager::lock_change_gates`] takes for the space.
    async fn write_sections(
        &self,
        space: &LinkSpace,
        urls: &[String],
        section: Option<&str>,
    ) -> bool {
        let mut written = true;
        for url in urls {
            if let Err(error) = self.write_progress_section(space, url, section).await {
                log!("progress section of {url}: {error:#}");
                written = false;
            }
        }
        written
    }

    /// Remove the progress section of the space's PRs at `urls`, which it
    /// marked ready, so they stop showing it; a failure is logged. The
    /// caller holds the gates [`Manager::lock_change_gates`] takes for the
    /// space.
    pub(crate) async fn remove_progress_sections(&self, space: &LinkSpace, urls: &[String]) {
        self.write_sections(space, urls, None).await;
    }

    /// The spaces whose progress changed after its sections last showed it.
    async fn unpublished_progress(&self) -> Result<Vec<String>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT space_id FROM space_progress
                         WHERE published_at IS NULL",
                    )?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }
}

#[cfg(test)]
mod tests;
