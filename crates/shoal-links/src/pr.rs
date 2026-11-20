//! The PRs a space assigns, as its PR watch record names them, and their
//! titles.
use rusqlite::{Connection, OptionalExtension};
use shoal_daemon_base::events::{self, EventKind};
use shoal_forge_types::items::{link::ItemKind, reference::ItemRef};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::pr::{Registration, RegistrationKind},
    model::Relation,
};

use crate::Links;

pub mod titles;

/// The PRs the space `id` watches; an unreadable registration watches none.
pub fn watched_prs(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<ItemRef>> {
    let registration = stored(db, id)?.and_then(|record| serde_json::from_str(&record).ok());
    Ok(watched_urls(registration.as_ref()).to_vec())
}

/// The PR registration of the space `id` as stored, which each reader
/// parses as it must take an unreadable one.
pub(crate) fn stored(db: &Connection, id: &SpaceId) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT record FROM pr_cleanup WHERE space_id=?1",
        [id],
        |row| row.get(0),
    )
    .optional()
}

/// The PRs `registration` watches, none without one.
pub fn watched_urls(registration: Option<&Registration>) -> &[ItemRef] {
    match registration {
        Some(Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) => urls,
        _ => &[],
    }
}

/// A space's PR registration replacing its previous one, with the title
/// the caller knows of the PR it watches.
pub struct RegistrationChange {
    registration: Option<Registration>,
    pub added: Vec<ItemRef>,
    removed: Vec<ItemRef>,
    title: Option<(ItemRef, String)>,
}

impl RegistrationChange {
    pub fn new(
        previous: Option<&Registration>,
        registration: Option<Registration>,
        title: Option<(ItemRef, String)>,
    ) -> Self {
        let before = watched_urls(previous);
        let after = watched_urls(registration.as_ref());
        let added = after
            .iter()
            .filter(|url| !before.contains(url))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|url| !after.contains(url))
            .cloned()
            .collect();
        Self {
            registration,
            added,
            removed,
            title,
        }
    }

    /// Store the registration within the caller's transaction. A dropped watch
    /// takes its replay cursors, review mark and title with it, an added one
    /// replaces the PR's opened or related link, and every watch added or
    /// dropped is journaled as an assigned PR link change. A title the caller
    /// knows replaces the recorded one.
    fn apply(&self, links: &Links, db: &Connection, id: &SpaceId) -> anyhow::Result<()> {
        links.forget_assigned(db, id, ItemKind::Pr, &self.removed)?;
        titles::forget(db, id, &self.removed)?;
        store_registration(db, id, self.registration.as_ref())?;
        for url in &self.added {
            crate::records::drop_recorded(db, id, url.url(), None)?;
            events::record_link(
                db,
                id,
                EventKind::Linked,
                Relation::Assigned,
                ItemKind::Pr,
                url.url(),
            )?;
        }
        if let Some((url, title)) = &self.title {
            titles::record(db, id, url.url(), title)?;
        }
        Ok(())
    }
}

fn store_registration(
    db: &Connection,
    id: &SpaceId,
    registration: Option<&Registration>,
) -> anyhow::Result<()> {
    match registration {
        Some(registration) => db.execute(
            "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)
             ON CONFLICT(space_id) DO UPDATE SET record=excluded.record",
            rusqlite::params![id, serde_json::to_string(registration)?],
        )?,
        None => db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [id])?,
    };
    Ok(())
}

impl Links {
    /// Apply `change` to the PR registration of the space `id` in a transaction
    /// that finds it ready: a removed space has no registration to change, and
    /// writing one would outlive it.
    pub fn record_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        id: &SpaceId,
        change: RegistrationChange,
    ) -> anyhow::Result<()> {
        self.records.require_ready_space(tx, id)?;
        change.apply(self, tx, id)
    }
}
