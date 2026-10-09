//! Move stacked workspaces down when their base workspace's PRs merge.
use anyhow::Result;
use rusqlite::params;

use super::{RegistrationKind, current_head, wait};
use crate::{
    daemon::{events::EventCause, workspace::Manager},
    forge::{
        link::ItemKind,
        updates::{Update, UpdateKind},
    },
    model::{Workspace, WorkspaceRef},
};

impl Manager {
    /// Once every watched PR of `base` has merged, retarget the PRs of the
    /// workspaces stacked on it to the branch it merged into, stack them on
    /// its own base, and queue a `base_merged` update for their next watch.
    /// `Ok(false)` while a failed lookup leaves the merge unknown: the base's
    /// cleanup waits for the next sweep without reporting a failure.
    pub(super) async fn advance_stack(
        &self,
        base: &Workspace,
        registration: &RegistrationKind,
    ) -> Result<bool> {
        let RegistrationKind::Watch { urls, .. } = registration else {
            return Ok(true);
        };
        if base.stacked_workspaces.is_empty() {
            return Ok(true);
        }
        for url in urls {
            match self.base_pr_merged(base, url).await {
                Ok(true) => {}
                Ok(false) => return Ok(true),
                Err(error) => {
                    eprintln!("could not check whether {url} merged: {error:#}");
                    return Ok(false);
                }
            }
        }
        let Some(url) = urls.last() else {
            return Ok(true);
        };
        let (forge, _, _) = self.pr_forge(base, url).await?;
        let target = forge.pull_request(&base.path, url).await?.base;
        let head = current_head(base).await?;
        for stacked in &base.stacked_workspaces {
            self.restack(stacked, base, &target, &head).await?;
        }
        Ok(true)
    }

    async fn base_pr_merged(&self, base: &Workspace, url: &str) -> Result<bool> {
        let (forge, number, _) = self.pr_forge(base, url).await?;
        Ok(forge
            .merged_commits(&base.path, number, &base.branch)
            .await?
            .is_some())
    }

    async fn restack(
        &self,
        stacked: &WorkspaceRef,
        base: &Workspace,
        target: &str,
        head: &str,
    ) -> Result<()> {
        let workspace = self.workspace(&stacked.id).await?;
        let id = workspace.id.clone();
        let items = self
            .store
            .run(move |db| wait::linked_items(db, &id, None))
            .await?;
        let mut updates = Vec::new();
        for (url, kind) in items {
            let retargeted = match kind {
                ItemKind::Pr => {
                    self.retarget_stacked(&workspace, &url, &base.branch, target)
                        .await
                }
                ItemKind::Issue => None,
            };
            let message = format!(
                "Base workspace {} merged into {target}{}. Rebase onto it: git fetch origin && git rebase --onto origin/{target} {head}",
                base.name,
                retargeted.unwrap_or_default()
            );
            updates.push(Update {
                url,
                kind: UpdateKind::BaseMerged,
                message,
                delivery: uuid::Uuid::new_v4().to_string(),
            });
        }
        let (id, base_id) = (workspace.id, base.id.clone());
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let changed = tx.execute(
                    "UPDATE workspaces SET base_workspace_id=(SELECT base_workspace_id FROM workspaces WHERE id=?2)
                     WHERE id=?1 AND base_workspace_id=?2",
                    params![id, base_id],
                )?;
                // Another sweep or a manual change already moved it.
                if changed == 0 {
                    return Ok(());
                }
                crate::daemon::workspace::record_base_changed(&tx, &id, EventCause::Pr)?;
                for update in updates {
                    wait::queue_update(&tx, &id, update)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Retarget a stacked PR that still targets the merged base branch,
    /// describing the outcome for the agent's update.
    async fn retarget_stacked(
        &self,
        workspace: &Workspace,
        url: &str,
        branch: &str,
        target: &str,
    ) -> Option<String> {
        let result = async {
            let (forge, number, _) = self.pr_forge(workspace, url).await?;
            if forge.pull_request(&workspace.path, url).await?.base != branch {
                return Ok(false);
            }
            forge.retarget(&workspace.path, number, target).await?;
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        match result {
            Ok(true) => Some(format!("; Shoal retargeted this PR to {target}")),
            Ok(false) => None,
            Err(error) => Some(format!(
                "; retargeting this PR to {target} failed: {error:#}"
            )),
        }
    }
}
