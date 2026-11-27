//! Clearing the blockers of each space's progress: those the forge says
//! cleared, spaces done at their current HEAD or removed, and issues done in
//! a workspace of the space's repository that links them, as admission
//! decides for a launch. A failed lookup keeps its blocker and records why.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Transaction;

use super::{Plan, stored, write};
use crate::{
    blockers::{Lookup, Lookups, Target},
    state::BlockerKind,
    time::UnixTime,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::events::BlockerEvent;

pub struct Cleared {
    pub space: SpaceId,
    pub cleared: Vec<BlockerEvent>,
}

struct Blocked {
    space: SpaceId,
    repository: Option<RepositoryId>,
    plan: Plan,
}

impl Spaces {
    pub async fn progress_forge_blockers(&self) -> Result<HashSet<Target>> {
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

    /// Journals each, and returns what changed of the spaces where any
    /// cleared.
    pub async fn clear_progress_blockers(
        &self,
        daemon: &dyn Daemon,
        lookups: &Lookups,
    ) -> Result<Vec<Cleared>> {
        let blocked = self.store.read(blocked_plans).await?;
        let mut changed = Vec::new();
        let mut failure = None;
        for space in blocked {
            match self.clear_space_blockers(daemon, space, lookups).await {
                Ok(Some(cleared)) => changed.push(cleared),
                Ok(None) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(changed), Err)
    }

    /// None unless any cleared.
    async fn clear_space_blockers(
        &self,
        daemon: &dyn Daemon,
        blocked: Blocked,
        lookups: &Lookups,
    ) -> Result<Option<Cleared>> {
        let mut found = Lookups::new();
        for (kind, target) in blocked.plan.targets() {
            let lookup = lookups.get(&(kind, target.clone())).cloned();
            let local = match lookup {
                Some(Lookup::Cleared(_)) => None,
                _ => (daemon.cleared_locally(blocked.repository.as_ref(), kind, &target)).await?,
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

fn blocked_plans(db: &Transaction<'_>) -> Result<Vec<Blocked>> {
    let rows = db
        .prepare(
            "SELECT p.space_id,s.repository_id FROM space_progress p
             LEFT JOIN spaces s ON s.id=p.space_id
             WHERE EXISTS (SELECT 1 FROM space_blockers b WHERE b.space_id=p.space_id)",
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

/// Journals each. None unless any cleared.
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
        crate::events::record_blocker(
            tx,
            id,
            shoal_protocol::events::EventKind::BlockerCleared,
            event,
        )?;
    }
    Ok((!cleared.is_empty()).then(|| Cleared {
        space: id.to_owned(),
        cleared,
    }))
}
