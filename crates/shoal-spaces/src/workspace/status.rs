use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::Space;
use anyhow::{Context, Result};
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::status::{DiffSummary, Inspection, WorktreeStatus};

use crate::{
    forge::{
        ForgeRepo,
        items::{link, reference::ItemInput},
    },
    git,
    model::{Selected, SelectedItem, Workspace},
    repository,
};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    pub async fn worktree_status(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<WorktreeStatus> {
        let Inspection {
            mut workspace,
            executions,
            places,
            ports,
            resources,
            mut simulators,
            ..
        } = self.inspect_workspace(daemon, selector).await?;
        let diff = self.file_access.checked(
            &self.notifications,
            &workspace.path,
            self.diff_summary(daemon, &workspace),
        );
        let (diff, diff_error) = match diff.await {
            Ok(diff) => (Some(diff), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let cleanup_error = self.workspace_cleanup_problem(&workspace.id);
        let workspace_id = workspace.id.clone();
        let setup_finished = self
            .store
            .run(move |db| setup_finished(db, &workspace_id))
            .await?;
        workspace.usage = self.workspace_usage(&workspace.id);
        workspace.job = daemon.workspace_job(&workspace.id).await?;
        simulators.retain(|simulator| simulator.workspace_id.as_ref() == Some(&workspace.id));
        let containers = daemon.container_project(&workspace.id).await?;
        let (leftover_count, leftovers) = self.leftovers.of(&workspace.id);
        let sync = daemon.checkout_sync(workspace.repository());
        // A worktree Git cannot read shows none, as `diff_error` says why.
        let changed = self.current_changed_on_both_sides(&workspace);
        let changed_on_both_sides = self
            .file_access
            .checked(&self.notifications, &workspace.path, changed)
            .await
            .ok()
            .flatten()
            .map(Box::new);
        let chats = self.chats_in(&workspace.id).await?;
        Ok(WorktreeStatus {
            workspace,
            executions,
            places,
            ports,
            resources,
            simulators,
            containers,
            setup_finished,
            diff,
            diff_error,
            cleanup_error,
            leftover_count,
            leftovers,
            sync,
            changed_on_both_sides,
            chats,
        })
    }
}

impl Spaces {
    async fn diff_summary(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<DiffSummary> {
        self.verify_worktree(daemon, workspace).await?;
        let base = self
            .diff_base(daemon, &SpaceSelector::id(&workspace.id))
            .await?;
        let numstat = git::diff::numstat(&workspace.path, base.commit.as_str()).await?;
        parse_numstat(&numstat)
    }
}

impl Spaces {
    /// In link order, each with the relations the space links it under. With
    /// `elsewhere`, an issue or PR URL of another repository selects that
    /// item.
    pub async fn select_items(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        selection: &link::Selection,
        elsewhere: bool,
    ) -> Result<Selected> {
        let origin = daemon.origin_path(space).await?;
        let other = match elsewhere {
            true => self.item_elsewhere(&origin, selection).await?,
            false => None,
        };
        let items = match other {
            Some(item) => vec![item],
            None => (daemon.selected_items(space, selection).await?)
                .into_iter()
                .map(|url| SelectedItem {
                    relations: space.links.relations_of(&url),
                    url,
                })
                .collect(),
        };
        Ok(Selected {
            path: Some(origin),
            items,
        })
    }

    /// None when the URL is of the repository whose origin `checkout` has.
    pub async fn item_elsewhere(
        &self,
        checkout: &std::path::Path,
        selection: &link::Selection,
    ) -> Result<Option<SelectedItem>> {
        let Some((forge, item)) = self.item_at_url(selection).await? else {
            return Ok(None);
        };
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        if forge == self.forges.forge(&remote).await? {
            return Ok(None);
        }
        Ok(Some(item))
    }

    /// In whichever repository, with that repository's forge.
    pub async fn item_at_url(
        &self,
        selection: &link::Selection,
    ) -> Result<Option<(ForgeRepo, SelectedItem)>> {
        let Some(ItemInput::Url(item)) = &selection.input else {
            return Ok(None);
        };
        let forge = self.forges.item_forge(item).await?;
        Ok(Some((
            forge,
            SelectedItem {
                url: item.clone(),
                relations: Vec::new(),
            },
        )))
    }
}

pub fn parse_numstat(output: &str) -> Result<DiffSummary> {
    let mut summary = DiffSummary {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in output.lines() {
        let mut fields = line.splitn(3, '\t');
        let insertions = fields.next().context("Git numstat omitted insertions")?;
        let deletions = fields.next().context("Git numstat omitted deletions")?;
        fields.next().context("Git numstat omitted the file name")?;
        summary.files_changed += 1;
        if insertions != "-" {
            summary.insertions += insertions
                .parse::<u64>()
                .context("invalid Git insertions")?;
        }
        if deletions != "-" {
            summary.deletions += deletions.parse::<u64>().context("invalid Git deletions")?;
        }
    }
    Ok(summary)
}

pub fn setup_finished(db: &Connection, workspace_id: &SpaceId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT setup_finished FROM workspaces WHERE id=?1",
        [workspace_id],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests;
