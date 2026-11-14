//! The areas ranking reads: those each workspace occupies, from the labels
//! of its assigned issues and PRs, and those of the issue each waiting
//! launch of a new workspace starts on, under its repository's
//! `[issue] area_prefix`.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::{
    super::rank::{Snapshot, Work},
    Settings,
};
use crate::client::{queue::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use crate::{
    daemon::{links::pr::watched_prs, store},
    forge::{items::reference::ItemRef, updates},
    state::{QueueKind, QueueState},
};

/// Work with labels in a repository.
pub(super) struct Labelled<Id> {
    pub id: Id,
    pub repository: RepositoryId,
    pub labels: Vec<String>,
}

/// The labels areas come from.
pub(super) struct Labels {
    /// Every workspace's, from its assigned issues and PRs.
    pub workspaces: Vec<Labelled<SpaceId>>,
    /// Each waiting launch's, from its issue, while it has no workspace.
    pub launches: Vec<Labelled<QueueEntryId>>,
}

impl Labels {
    /// The repositories the labels belong to.
    pub(super) fn repositories(&self) -> impl Iterator<Item = RepositoryId> + '_ {
        let workspaces = self.workspaces.iter().map(|space| &space.repository);
        let launches = self.launches.iter().map(|launch| &launch.repository);
        workspaces.chain(launches).cloned()
    }
}

/// The labels of every workspace, in the order of their numbers, and of
/// each waiting launch that has none yet.
pub(super) fn labels(db: &Connection) -> Result<Labels> {
    Ok(Labels {
        workspaces: workspaces(db)?,
        launches: launches(db)?,
    })
}

/// Every workspace with the labels of its assigned issues and PRs, as the
/// background poll, or else a watch, last read them.
fn workspaces(db: &Connection) -> Result<Vec<Labelled<SpaceId>>> {
    let rows = db
        .prepare("SELECT id,repository_id FROM workspaces ORDER BY number IS NULL,number,id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(SpaceId, RepositoryId)>>>()?;
    (rows.into_iter())
        .map(|(id, repository)| {
            Ok(Labelled {
                labels: assigned_labels(db, &id)?,
                id,
                repository,
            })
        })
        .collect()
}

/// The labels of the issues and PRs assigned to the workspace `id`.
fn assigned_labels(db: &Connection, id: &SpaceId) -> Result<Vec<String>> {
    let issues = store::assigned_issues(db, id)?.into_iter();
    let items = issues.map(|issue| issue.url).chain(watched_prs(db, id)?);
    let mut labels = Vec::new();
    for url in items {
        labels.extend(item_labels(db, id, &url)?);
    }
    Ok(labels)
}

/// The labels of the space `id`'s item `url`; none before they are read
/// or when its record does not read, rather than failing the rank.
fn item_labels(db: &Connection, id: &SpaceId, url: &ItemRef) -> Result<Vec<String>> {
    let record: Option<String> = db.query_row(
        "SELECT COALESCE(
            (SELECT record FROM space_item_activity WHERE space_id=?1 AND url=?2),
            (SELECT record FROM pr_activity WHERE space_id=?1 AND url=?2))",
        params![id, url],
        |row| row.get(0),
    )?;
    Ok((record.as_deref())
        .and_then(|record| updates::saved_labels(record).ok())
        .unwrap_or_default())
}

/// The waiting launches without a workspace, with the labels of their
/// issues, as they were queued with them.
fn launches(db: &Connection) -> Result<Vec<Labelled<QueueEntryId>>> {
    let rows = db
        .prepare(
            "SELECT id,repository_id,issue_labels FROM agent_queue
             WHERE kind=?1 AND state=?2 AND space_id IS NULL AND opens_workspace_id IS NULL
                AND repository_id IS NOT NULL AND issue_labels IS NOT NULL",
        )?
        .query_map(params![QueueKind::Launch, QueueState::Queued], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
    Ok((rows.into_iter())
        .map(|(id, repository, labels)| Labelled {
            id,
            repository,
            labels: serde_json::from_str(&labels).unwrap_or_default(),
        })
        .collect())
}

/// Give `snapshot` the areas of `labels` under each repository's area
/// prefix in `settings`: those its workspaces occupy, and each waiting
/// launch of a new workspace its own. A repository without settings or a
/// prefix has none.
pub(super) fn place(snapshot: &mut Snapshot, labels: Labels, settings: &Settings) {
    for workspace in &labels.workspaces {
        for area in areas(workspace, settings) {
            let occupied = snapshot.occupied.entry(workspace.repository.clone());
            let holders = occupied.or_default().entry(area).or_default();
            holders.push(workspace.id.clone());
        }
    }
    for launch in &labels.launches {
        let entry = (snapshot.entries.iter_mut()).find(|entry| {
            entry.id == launch.id && matches!(entry.work, Work::Launch { space: None })
        });
        if let Some(entry) = entry {
            entry.areas = areas(launch, settings);
        }
    }
}

/// The areas among the labels of `labelled` under its repository's area
/// prefix in `settings`.
fn areas<Id>(labelled: &Labelled<Id>, settings: &Settings) -> Vec<String> {
    let resolved = settings.get(&labelled.repository).and_then(Option::as_ref);
    let labels = labelled.labels.iter().map(String::as_str);
    resolved.map_or_else(Vec::new, |resolved| resolved.issue.areas(labels))
}
