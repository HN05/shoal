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
    /// A failed merge lookup returns its error as `Ok(Some(_))`: the base's
    /// cleanup waits for the next sweep without reporting a failure.
    pub(super) async fn advance_stack(
        &self,
        base: &Workspace,
        registration: &RegistrationKind,
    ) -> Result<Option<anyhow::Error>> {
        let RegistrationKind::Watch { urls, .. } = registration else {
            return Ok(None);
        };
        if base.stacked_workspaces.is_empty() {
            return Ok(None);
        }
        // Its HEAD and remote authorize forge writes for other workspaces.
        self.verify_worktree(base).await?;
        let mut merged = Vec::new();
        for url in urls {
            match self.base_pr_commits(base, url).await {
                Ok(Some(commits)) => merged.extend(commits),
                Ok(None) => return Ok(None),
                Err(error) => {
                    return Ok(Some(
                        error.context(format!("could not check whether {url} merged")),
                    ));
                }
            }
        }
        let Some(url) = urls.last() else {
            return Ok(None);
        };
        // The rebase cutoff is HEAD, so commits the merge left out would be
        // dropped from every stacked branch.
        let head = current_head(base).await?;
        if !merged.contains(&head) {
            return Ok(Some(anyhow::anyhow!(
                "the merged PRs do not contain the base workspace's HEAD"
            )));
        }
        let (forge, _, _) = self.pr_forge(base, url).await?;
        let target = forge.pull_request(&base.path, url).await?.base;
        // Reload: this sweep may already have removed or moved some of them.
        for stacked in self.workspace(&base.id).await?.stacked_workspaces {
            self.restack(&stacked, base, &target, &head).await?;
        }
        Ok(None)
    }

    async fn base_pr_commits(&self, base: &Workspace, url: &str) -> Result<Option<Vec<String>>> {
        let (forge, number, _) = self.pr_forge(base, url).await?;
        forge.merged_commits(&base.path, number, &base.branch).await
    }

    async fn restack(
        &self,
        stacked: &WorkspaceRef,
        base: &Workspace,
        target: &str,
        head: &str,
    ) -> Result<()> {
        let workspace = self.workspace(&stacked.id).await?;
        self.verify_worktree(&workspace).await?;
        let id = workspace.id.clone();
        let items = self
            .store
            .run(move |db| wait::linked_items(db, &id, None))
            .await?;
        let mut updates = Vec::new();
        for (url, kind) in items {
            let retargeted = match kind {
                ItemKind::Pr => {
                    self.retarget_stacked(&workspace, &url, base, head, target)
                        .await?
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
        self.ensure_base_unchanged(base, head).await?;
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
    /// describing the outcome for the agent's update. Changed ownership or
    /// base HEAD aborts the restack instead.
    async fn retarget_stacked(
        &self,
        workspace: &Workspace,
        url: &str,
        base: &Workspace,
        head: &str,
        target: &str,
    ) -> Result<Option<String>> {
        let lookup = async {
            let (forge, number, _) = self.pr_forge(workspace, url).await?;
            let current = forge.pull_request(&workspace.path, url).await?.base;
            Ok::<_, anyhow::Error>((current == base.branch).then_some((forge, number)))
        }
        .await;
        let (forge, number) = match lookup {
            Ok(Some(pr)) => pr,
            Ok(None) => return Ok(None),
            Err(error) => {
                return Ok(Some(format!(
                    "; retargeting this PR to {target} failed: {error:#}"
                )));
            }
        };
        // Lookups take time; recheck what authorizes the write right before it.
        self.verify_worktree(workspace).await?;
        self.ensure_base_unchanged(base, head).await?;
        Ok(Some(match forge.retarget(number, target).await {
            Ok(()) => format!("; Shoal retargeted this PR to {target}"),
            Err(error) => format!("; retargeting this PR to {target} failed: {error:#}"),
        }))
    }

    /// The rebase cutoff in the update is the base HEAD read before restacking.
    async fn ensure_base_unchanged(&self, base: &Workspace, head: &str) -> Result<()> {
        self.verify_worktree(base).await?;
        anyhow::ensure!(
            current_head(base).await? == head,
            "the base workspace's HEAD changed while restacking; retrying next sweep"
        );
        Ok(())
    }
}
