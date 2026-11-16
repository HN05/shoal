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
        let previous = self.space(selector).await?;
        let id = previous.id().clone();
        self.store
            .write(move |tx| store_name(tx, &id, &name))
            .await?;
        Ok(RenamedSpace {
            space: self.space_by_id(previous.id()).await?,
            previous_name: previous.name().clone(),
        })
    }
}

/// Record `name` as the name of space `id`.
fn store_name(tx: &Transaction<'_>, id: &SpaceId, name: &SpaceName) -> Result<()> {
    tx.execute("UPDATE spaces SET name=?2 WHERE id=?1", params![id, name])?;
    Ok(())
}

#[cfg(test)]
mod tests;
