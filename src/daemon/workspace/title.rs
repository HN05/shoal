//! Workspace titles: what a workspace's agent calls its work, which the
//! places hosting the workspace show.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::Manager;
use crate::model::Workspace;

impl Manager {
    /// Give the workspace `selector` names `title`, or clear it without one.
    pub async fn set_workspace_title(
        &self,
        selector: &str,
        title: Option<String>,
    ) -> Result<Workspace> {
        let title = title.map(|title| title.trim().to_owned());
        if let Some(title) = &title {
            crate::validate::title(title)?;
        }
        let id = self.workspace(selector).await?.id;
        let titled = id.clone();
        self.store
            .run(move |db| store_title(db, &titled, title.as_deref()))
            .await?;
        self.workspace(&id).await
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
