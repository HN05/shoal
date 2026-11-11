//! A workspace's name, the label its lists, tabs and notices show, which
//! only `shoal rename` changes after its creation.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::{Connection, params};

use super::Manager;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::model::RenamedWorkspace;

impl Manager {
    /// Give the workspace `selector` names `name`, returning it with the
    /// name it had before, which its places may still show.
    pub async fn set_workspace_name(
        &self,
        selector: &SpaceSelector,
        name: &SpaceName,
    ) -> Result<RenamedWorkspace> {
        // A request's name reads as sent, so the rule applies here.
        let name = SpaceName::parse(name)?;
        let previous = self.workspace(selector).await?;
        let id = previous.id.clone();
        self.store.run(move |db| store_name(db, &id, &name)).await?;
        Ok(RenamedWorkspace {
            workspace: self.workspace_by_id(&previous.id).await?,
            previous_name: previous.name,
        })
    }
}

/// Record `name` as the name of workspace `id`.
fn store_name(db: &mut Connection, id: &SpaceId, name: &SpaceName) -> Result<()> {
    db.execute(
        "UPDATE workspaces SET name=?2 WHERE id=?1",
        params![id, name],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
