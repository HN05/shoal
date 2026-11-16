//! A space's name, the label its lists, tabs and notices show, which only
//! `shoal rename` changes after its creation.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;

use crate::{daemon::workspace::Manager, model::RenamedSpace};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

impl Manager {
    /// Give the workspace or chat `selector` names the name `name`,
    /// returning it with the name it had before, which its places may still
    /// show.
    pub async fn rename_space(
        &self,
        selector: &SpaceSelector,
        name: &SpaceName,
    ) -> Result<RenamedSpace> {
        // A request's name reads as sent, so the rule applies here.
        let name = SpaceName::parse(name)?;
        let id = self.space(selector).await?.id.clone();
        let owned = id.clone();
        let previous_name = (self.store)
            .write(move |tx| store_name(tx, &owned, &name))
            .await?;
        Ok(RenamedSpace {
            space: self.space_by_id(&id).await?,
            previous_name,
        })
    }
}

/// Record `name` as the name of space `id`, returning the name it replaces,
/// read in the same transaction so a concurrent rename cannot come between.
fn store_name(tx: &Transaction<'_>, id: &SpaceId, name: &SpaceName) -> Result<SpaceName> {
    let previous = tx.query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
        row.get(0)
    })?;
    tx.execute("UPDATE spaces SET name=?2 WHERE id=?1", params![id, name])?;
    Ok(previous)
}

#[cfg(test)]
mod tests;
