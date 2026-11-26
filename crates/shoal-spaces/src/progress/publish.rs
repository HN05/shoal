//! Showing each space's progress in its open draft PRs. See
//! `docs/design/progress.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::Records;
use crate::forge::items::reference::ItemRef;
use crate::model::Space;
use shoal_ids::space_ids::SpaceId;
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
    forge::items::{
        action::{Action, Edit},
        item::ItemState,
        link::ItemKind,
    },
    log,
    model::Relation,
};
use shoal_forge::items::item::ItemRequests;
use shoal_protocol::space_names::SpaceSelector;

pub const INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Publications {
    requested: Mutex<HashSet<SpaceId>>,
    wake: Notify,
}

impl Publications {
    pub fn request(&self, id: &SpaceId) {
        self.lock().insert(id.clone());
        self.wake.notify_one();
    }

    pub fn take(&self) -> Vec<SpaceId> {
        self.lock().drain().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<SpaceId>> {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
pub struct Schedule {
    due: HashMap<SpaceId, Instant>,
    published: HashMap<SpaceId, Instant>,
}

impl Schedule {
    /// A space already due keeps its time, and publishes what it has then.
    pub fn request(&mut self, id: SpaceId, now: Instant) {
        let earliest =
            (self.published.get(&id)).map_or(now, |published| now.max(*published + INTERVAL));
        self.due.entry(id).or_insert(earliest);
    }

    pub fn next(&self) -> Option<Instant> {
        self.due.values().min().copied()
    }

    pub fn take_due(&mut self, now: Instant) -> Vec<SpaceId> {
        let due: Vec<SpaceId> = (self.due.iter())
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

/// Publishes what changed while the daemon was down first.
pub async fn run(daemon: Arc<dyn Daemon>) {
    match daemon.spaces().unpublished_progress().await {
        Ok(spaces) => spaces
            .iter()
            .for_each(|id| daemon.spaces().publications.request(id)),
        Err(error) => log!("progress sections: {error:#}"),
    }
    let mut schedule = Schedule::default();
    loop {
        let now = Instant::now();
        for id in daemon.spaces().publications.take() {
            schedule.request(id, now);
        }
        for id in schedule.take_due(now) {
            if let Err(error) = daemon.spaces().publish_progress(&*daemon, &id).await {
                log!("progress sections of space {id}: {error:#}");
            }
        }
        let next = schedule.next();
        tokio::select! {
            _ = daemon.spaces().publications.wake.notified() => {}
            _ = tokio::time::sleep_until(next.unwrap_or(now)), if next.is_some() => {}
        }
    }
}

/// They show the progress while they are open drafts.
fn publishing_pulls(db: &Connection, records: &dyn Records, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let assigned = records
        .assigned_items(db, id, Some(ItemKind::Pr))?
        .into_iter();
    let opened = (records
        .recorded_items(db, id, Relation::Opened)?
        .into_iter())
    .filter(|item| item.kind() == ItemKind::Pr.into())
    .map(|item| item.url);
    Ok(assigned.chain(opened).collect())
}

impl Spaces {
    /// Removes the section from the space's PRs that are not drafts. Does
    /// nothing while `[pr] progress` is off. Records the progress as
    /// published once every PR shows it.
    pub async fn publish_progress(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let space = self.space(&SpaceSelector::id(id)).await?;
        if !daemon.acting_settings(&space).await?.pr.progress {
            return Ok(());
        }
        let _gates = daemon.lock_change_gates(id, &[]).await?;
        let (query, records) = (id.to_owned(), self.records.clone());
        let (stored, urls) = (self.store)
            .read(move |db| {
                Ok((
                    stored(db, &query)?,
                    publishing_pulls(db, &*records, &query)?,
                ))
            })
            .await?;
        let Some((plan, updated_at)) = stored else {
            return Ok(());
        };
        let section = (!plan.is_empty()).then(|| section::render(&plan.progress(updated_at)));
        if !self
            .write_sections(daemon, &space, &urls, section.as_deref())
            .await
        {
            return Ok(());
        }
        let query = id.to_owned();
        (self.store)
            .write(move |db| mark_published(db, &query, &plan))
            .await
    }

    /// `None` removes the section from every PR. Logs each PR that fails
    /// without stopping at it, and returns whether every one succeeded. The
    /// caller holds the gates [`shoal_links::Links::lock_change_gates`]
    /// takes for the space.
    async fn write_sections(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        urls: &[ItemRef],
        section: Option<&str>,
    ) -> bool {
        let mut written = true;
        for url in urls {
            if let Err(error) = self.write_section(daemon, space, url, section).await {
                log!("progress section of {url}: {error:#}");
                written = false;
            }
        }
        written
    }

    pub async fn unpublished_progress(&self) -> Result<Vec<SpaceId>> {
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

    /// Writes only to a draft, and removes the section for `None` or a PR out
    /// of draft, as the agent account. Skips a PR that is no longer open or
    /// already shows it. The caller holds the gates
    /// [`shoal_links::Links::lock_change_gates`] takes for the space.
    async fn write_section(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        pull: &ItemRef,
        section: Option<&str>,
    ) -> Result<()> {
        let forge = daemon.space_forge(space).await?;
        let account = self.forges.agent_account();
        pull.belongs_to(&forge)?;
        let (number, url) = (pull.pull()?, pull.to_string());
        let current = forge.read(&account, ItemKind::Pr, number.get()).await?;
        if current.item.state != ItemState::Open {
            return Ok(());
        }
        let draft = current.item.pr.as_ref().is_some_and(|pr| pr.draft);
        let body = section::with_section(&current.body, section.filter(|_| draft));
        if body == current.body {
            return Ok(());
        }
        let action = Action::edit(Edit {
            body: Some(body),
            ..Edit::default()
        });
        let item = (ItemKind::Pr, number.get(), url);
        let applied = daemon.apply_own(&space.id, &forge, &account, item, &action);
        applied.await?;
        Ok(())
    }
}
