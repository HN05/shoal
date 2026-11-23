//! What editors report: the files the user edited or saved in a worktree,
//! which make a change an agent's command made to the same file while it
//! ran mixed, since either may have made it.
use crate::{Coordination, daemon::Daemon};
use std::path::PathBuf;

use anyhow::{Result, ensure};

use super::records;
use crate::{model::Workspace, scope::Caller, threads, time::UnixTime};

impl Coordination {
    /// The user edited or saved the files `paths`, by absolute path, each
    /// in the worktree of a space `caller`'s scope reaches; a path in no
    /// worktree is left out.
    pub async fn report_edits(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
        let workspaces = daemon.list_workspaces().await?;
        let mut edited = Vec::new();
        for path in paths {
            let path = threads::canonical(&path);
            let Some(workspace) = Workspace::innermost(&workspaces, &path) else {
                continue;
            };
            if let Some(caller) = caller {
                ensure!(
                    self.reaches(daemon, caller, &workspace.id).await?,
                    "{} is in workspace {}, which your scope does not reach",
                    path.display(),
                    workspace.name
                );
            }
            let relative = threads::relative(&threads::canonical(&workspace.path), &path)?;
            edited.push((workspace.id.clone(), relative));
        }
        let at = UnixTime::now();
        (self.store)
            .write(move |tx| {
                for (space, path) in &edited {
                    records::report_edit(tx, space, path, at)?;
                }
                Ok(())
            })
            .await
    }
}
