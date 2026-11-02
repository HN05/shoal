use anyhow::{Context, Result};

use super::Manager;
use crate::{
    daemon::{spaces::status::Records, store},
    forge::{IssueInput, items::link},
    git,
    model::{DiffSummary, Inspection, Selected, SelectedItem, Workspace, WorktreeStatus},
    repository,
};

impl Manager {
    /// What the status of the workspace `selector` names shows for its
    /// worktree, and what the workspace records about its links and work.
    pub(crate) async fn worktree_status(
        &self,
        selector: &str,
    ) -> Result<(WorktreeStatus, Records)> {
        let Inspection {
            issue,
            completion,
            pr_cleanup,
            mut workspace,
            executions,
            places,
            ports,
            resources,
            mut simulators,
        } = self.inspect_workspace(selector).await?;
        let diff = self.check_access(&workspace.path, self.diff_summary(&workspace));
        let (diff, diff_error) = match diff.await {
            Ok(diff) => (Some(diff), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let cleanup_error = self.workspace_cleanup_problem(&workspace.id);
        let cleanup_reason = self.cleanup_reason(&workspace).await;
        let cleanup_at = self.cleanup_at(&workspace.id).await?;
        let workspace_id = workspace.id.clone();
        let (setup_finished, turn_retry) = self
            .store
            .run(move |db| {
                Ok((
                    store::setup_finished(db, &workspace_id)?,
                    super::turn_retry::get(db, &workspace_id)?,
                ))
            })
            .await?;
        workspace.usage = self.workspace_usage(&workspace.id);
        let proxy_retry = self.proxy_retry(&executions);
        simulators
            .retain(|simulator| simulator.workspace_id.as_deref() == Some(workspace.id.as_str()));
        let (leftover_count, leftovers) = self.leftovers.of(&workspace.id);
        let sync = self.checkout_sync(&workspace.repository_id);
        // A worktree Git cannot read shows none, as `diff_error` says why.
        let changed = self.current_changed_on_both_sides(&workspace);
        let changed_on_both_sides = self
            .check_access(&workspace.path, changed)
            .await
            .ok()
            .flatten()
            .map(Box::new);
        let status = WorktreeStatus {
            workspace,
            executions,
            places,
            ports,
            resources,
            simulators,
            setup_finished,
            diff,
            diff_error,
            cleanup_error,
            cleanup_reason,
            cleanup_at,
            turn_retry,
            proxy_retry,
            leftover_count,
            leftovers,
            sync,
            changed_on_both_sides,
        };
        let records = Records {
            issue,
            pr_cleanup,
            completion,
        };
        Ok((status, records))
    }
}

impl Manager {
    /// What the workspace changed since its base.
    async fn diff_summary(&self, workspace: &Workspace) -> Result<DiffSummary> {
        self.verify_worktree(workspace).await?;
        let base = self.diff_base(&workspace.id).await?;
        let numstat = git::diff::numstat(&workspace.path, &base.commit).await?;
        parse_numstat(&numstat)
    }
}

impl Manager {
    /// The selected items in link order, with the worktree to look them up
    /// from. With `elsewhere`, an issue or PR URL of another repository
    /// selects that item.
    pub async fn select_items(
        &self,
        selector: &str,
        selection: &link::Selection,
        elsewhere: bool,
    ) -> Result<Selected> {
        let space = self.link_space(selector).await?;
        let other = match elsewhere {
            true => self.item_elsewhere(space.origin_path(), selection).await?,
            false => None,
        };
        let items = match other {
            Some(item) => vec![item],
            None => (self.selected_items(&space, selection).await?)
                .into_iter()
                .map(|(url, kind)| SelectedItem { url, kind })
                .collect(),
        };
        Ok(Selected {
            path: space.origin_path().to_owned(),
            items,
        })
    }

    /// The issue, PR or CI run whose URL `selection` names, when it is of
    /// another repository than the one whose origin `checkout` has.
    async fn item_elsewhere(
        &self,
        checkout: &std::path::Path,
        selection: &link::Selection,
    ) -> Result<Option<SelectedItem>> {
        let (Some(kind), Some(input)) = (selection.kind, selection.input.as_deref()) else {
            return Ok(None);
        };
        if IssueInput::parse(input) != IssueInput::Url {
            return Ok(None);
        }
        let (forge, url) = match kind {
            link::LinkKind::Issue => {
                let forge = self.issue_url_forge(input).await?;
                let (_, url) = forge.issue(input)?;
                (forge, url)
            }
            link::LinkKind::Pr => {
                let forge = self.pull_forge(input).await?;
                let (_, url) = forge.pull(input)?;
                (forge, url)
            }
            link::LinkKind::Ci => {
                let forge = self.run_url_forge(input).await?;
                let (_, url) = forge.run(input)?;
                (forge, url)
            }
        };
        let remote = repository::remote_url_from_path(checkout)
            .await?
            .context("item lookup needs an origin remote")?;
        if forge == self.forge(&remote).await? {
            return Ok(None);
        }
        Ok(Some(SelectedItem { url, kind }))
    }
}

fn parse_numstat(output: &str) -> Result<DiffSummary> {
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

#[cfg(test)]
mod tests;
