//! The priority set on a space, which ranks its work in the agent queue
//! ahead of everything else.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::{scope::Caller, workspace::Manager};
use crate::client::{
    priority::{Prioritized, Priority},
    space_ids::SpaceId,
    space_names::SpaceSelector,
};

/// The priority of the space `id`: the one set on it, else the default.
pub(crate) fn current(db: &Connection, id: &SpaceId) -> Result<Priority> {
    let set: Option<Priority> = db
        .query_row(
            "SELECT priority FROM space_priority WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(set.unwrap_or_default())
}

/// Set `priority` on the space `id`.
pub(crate) fn set(db: &Connection, id: &SpaceId, priority: Priority) -> Result<()> {
    db.execute(
        "INSERT INTO space_priority(space_id,priority) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET priority=excluded.priority",
        params![id, priority],
    )?;
    Ok(())
}

impl Manager {
    /// Set `priority` on the space `selector` names for `caller`, whose
    /// scope reaches it.
    pub(crate) async fn set_priority(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        priority: Priority,
    ) -> Result<Prioritized> {
        let space = self.space(selector).await?;
        self.given_priority(caller, Some(priority)).await?;
        self.give_priority(space.id(), Some(priority)).await?;
        Ok(Prioritized {
            space: space.name().to_owned(),
            priority,
        })
    }

    /// The priority `caller` gives a space it creates or ranks, asking for
    /// `priority`: what it asks, or with none the default. A caller whose
    /// scope has a root of its own gives at most its own space's, as it
    /// gives only permissions it holds, so asking for more is refused and
    /// asking for none gives its own when that is lower; the user and a
    /// master session give any.
    pub(crate) async fn given_priority(
        &self,
        caller: Option<&Caller>,
        priority: Option<Priority>,
    ) -> Result<Option<Priority>> {
        let Some(root) = self.reach_root(caller).await? else {
            return Ok(priority);
        };
        let own = self.store.read(move |tx| current(tx, &root)).await?;
        match priority {
            Some(priority) => {
                ensure!(
                    priority <= own,
                    "scoped processes give a priority of at most their own space's, {own}"
                );
                Ok(Some(priority))
            }
            None => Ok((own < Priority::default()).then_some(own)),
        }
    }

    /// Set `priority`, when given, on the space `id`.
    pub(crate) async fn give_priority(
        &self,
        id: &SpaceId,
        priority: Option<Priority>,
    ) -> Result<()> {
        let Some(priority) = priority else {
            return Ok(());
        };
        let id = id.clone();
        self.store.write(move |db| set(db, &id, priority)).await
    }
}

#[cfg(test)]
mod tests;
