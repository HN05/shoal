//! Clearing the blockers of each space's progress: those the forge says
//! cleared, spaces done at their current HEAD or removed, and issues done in
//! a workspace of the space's repository that links them, as admission
//! decides for a launch. A failed lookup keeps its blocker and records why.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Transaction;

use super::{Plan, stored, write};
use crate::{
    client::{events::BlockerEvent, repository_ids::RepositoryId, space_ids::SpaceId},
    daemon::{
        blockers::{Lookup, Lookups, Target},
        workspace::Manager,
    },
    state::BlockerKind,
    time::UnixTime,
};

/// What clearing changed of a space's blockers.
pub(crate) struct Cleared {
    pub space: SpaceId,
    /// The blockers that cleared.
    pub cleared: Vec<BlockerEvent>,
}

/// A space whose progress has blockers, with its repository.
struct Blocked {
    space: SpaceId,
    repository: Option<RepositoryId>,
    plan: Plan,
}

impl Manager {
    /// The issues, PRs and CI runs that block spaces or their steps.
    pub(crate) async fn progress_forge_blockers(&self) -> Result<HashSet<Target>> {
        let blocked = self.store.read(blocked_plans).await?;
        Ok((blocked.iter())
            .flat_map(|blocked| blocked.plan.targets())
            .filter(|(kind, _)| {
                matches!(
                    kind,
                    BlockerKind::Issue | BlockerKind::Pr | BlockerKind::CiRun
                )
            })
            .collect())
    }

    /// Clear the blockers of every space that `lookups` or the spaces they
    /// name say cleared, journaling each, and return what changed of the
    /// spaces where any cleared.
    pub(crate) async fn clear_progress_blockers(&self, lookups: &Lookups) -> Result<Vec<Cleared>> {
        let blocked = self.store.read(blocked_plans).await?;
        let mut changed = Vec::new();
        let mut failure = None;
        for space in blocked {
            match self.clear_space_blockers(space, lookups).await {
                Ok(Some(cleared)) => changed.push(cleared),
                Ok(None) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(changed), Err)
    }

    /// Clear the blockers of `blocked` that cleared; what changed when any
    /// did.
    async fn clear_space_blockers(
        &self,
        blocked: Blocked,
        lookups: &Lookups,
    ) -> Result<Option<Cleared>> {
        let mut found = Lookups::new();
        for (kind, target) in blocked.plan.targets() {
            let lookup = lookups.get(&(kind, target.clone())).cloned();
            let local = match lookup {
                Some(Lookup::Cleared(_)) => None,
                _ => (self.cleared_locally(blocked.repository.as_ref(), kind, &target)).await?,
            };
            if let Some(found_now) = local.map(Lookup::Cleared).or(lookup) {
                found.insert((kind, target), found_now);
            }
        }
        let id = blocked.space.clone();
        let now = UnixTime::now();
        let cleared = (self.store)
            .write(move |tx| clear(tx, &id, &found, now))
            .await?;
        if cleared.is_some() {
            self.publications.request(&blocked.space);
        }
        Ok(cleared)
    }
}

/// Every space whose progress has blockers, with its repository.
fn blocked_plans(db: &Transaction<'_>) -> Result<Vec<Blocked>> {
    let rows = db
        .prepare(
            "SELECT p.space_id,COALESCE(w.repository_id,c.repository_id) FROM space_progress p
             LEFT JOIN workspaces w ON w.id=p.space_id LEFT JOIN chats c ON c.id=p.space_id
             WHERE json_array_length(p.record,'$.blockers') > 0",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(SpaceId, Option<RepositoryId>)>>>()?;
    let mut blocked = Vec::new();
    for (space, repository) in rows {
        if let Some((plan, _)) = stored(db, &space)? {
            blocked.push(Blocked {
                space,
                repository,
                plan,
            });
        }
    }
    Ok(blocked)
}

/// Remove the blockers of the space `id` that `found` says cleared at
/// `now`, journaling each; what changed when any did.
fn clear(
    tx: &Transaction<'_>,
    id: &SpaceId,
    found: &Lookups,
    now: UnixTime,
) -> Result<Option<Cleared>> {
    let Some((before, _)) = stored(tx, id)? else {
        return Ok(None);
    };
    let (after, cleared) = before.clone().cleared(found);
    if write(tx, id, &before, after, &[], now)?.is_none() {
        return Ok(None);
    }
    for event in &cleared {
        crate::daemon::events::record_blocker(
            tx,
            id,
            crate::client::events::EventKind::BlockerCleared,
            event,
        )?;
    }
    Ok((!cleared.is_empty()).then(|| Cleared {
        space: id.to_owned(),
        cleared,
    }))
}
