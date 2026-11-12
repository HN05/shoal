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
    /// scope reaches it: a caller whose scope has a root of its own gives at
    /// most its own space's priority, as it gives only permissions it holds.
    pub(crate) async fn set_priority(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        priority: Priority,
    ) -> Result<Prioritized> {
        let space = self.space(selector).await?;
        let root = self.reach_root(caller).await?;
        let id = space.id().to_owned();
        (self.store)
            .write(move |db| {
                if let Some(root) = &root {
                    let own = current(db, root)?;
                    ensure!(
                        priority <= own,
                        "scoped processes give a priority of at most their own space's, {own}"
                    );
                }
                set(db, &id, priority)
            })
            .await?;
        Ok(Prioritized {
            space: space.name().to_owned(),
            priority,
        })
    }
}

#[cfg(test)]
mod tests;
