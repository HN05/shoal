//! Workspace titles: what a workspace's agent calls its work, which the
//! places hosting the workspace show.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::Manager;
use crate::model::RetitledWorkspace;

impl Manager {
    /// Give the workspace `selector` names `title`, or clear it without one,
    /// returning it with the title it had before, which its places may
    /// still show.
    pub async fn set_workspace_title(
        &self,
        selector: &str,
        title: Option<String>,
    ) -> Result<RetitledWorkspace> {
        let title = title.map(|title| title.trim().to_owned());
        if let Some(title) = &title {
            crate::validate::title(title)?;
        }
        let previous = self.workspace(selector).await?;
        let titled = previous.id.clone();
        self.store
            .run(move |db| store_title(db, &titled, title.as_deref()))
            .await?;
        Ok(RetitledWorkspace {
            workspace: self.workspace(&previous.id).await?,
            previous_title: previous.title,
        })
    }
}

/// Record `title` as the title of workspace `id`, or clear it without one.
fn store_title(db: &mut Connection, id: &str, title: Option<&str>) -> Result<()> {
    db.execute(
        "UPDATE workspaces SET title=?2 WHERE id=?1",
        params![id, title],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
