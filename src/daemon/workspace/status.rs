use anyhow::{Context, Result};

use super::Manager;
use crate::{
    daemon::{forge_hosts::ForgeHosts, links::pr, store},
    forge::{ForgeRepo, IssueInput, account::Account, items::link},
    git,
    model::{
        DiffSummary, Inspection, IssueStatus, Selected, SelectedItem, Workspace, WorkspaceStatus,
    },
    repository,
};

impl Manager {
    pub async fn workspace_status(&self, selector: &str) -> Result<WorkspaceStatus> {
        let mut inspection = self.inspect_workspace(selector).await?;
        let path = inspection.workspace.path.clone();
        let diff = self.check_access(&path, self.diff_summary(&inspection.workspace));
        let (diff, diff_error) = match diff.await {
            Ok(diff) => (Some(diff), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let remote = self
            .check_access(&path, repository::remote_url_from_path(&path))
            .await;
        let account = self.user_account();
        let hosts = self.forge_hosts();
        let (prs, issue_status) = tokio::join!(
            pr::watched(&hosts, &account, &remote, inspection.pr_cleanup.as_ref()),
            issue_status(&hosts, &account, &remote, &inspection)
        );
        let unread_notifications = self
            .unread_workspace_notifications(&inspection.workspace.id)
            .await?;
        let cleanup_error = self.workspace_cleanup_problem(&inspection.workspace.id);
        let cleanup_reason = self.cleanup_reason(&inspection.workspace).await;
        let cleanup_at = self.cleanup_at(&inspection.workspace.id).await?;
        let workspace_id = inspection.workspace.id.clone();
        let (setup_finished, turn_retry) = self
            .store
            .run(move |db| {
                Ok((
                    store::setup_finished(db, &workspace_id)?,
                    super::turn_retry::get(db, &workspace_id)?,
                ))
            })
            .await?;
        inspection.workspace.usage = self.workspace_usage(&inspection.workspace.id);
        let proxy_retry = self.proxy_retry(&inspection.executions);
        inspection.simulators.retain(|simulator| {
            simulator.workspace_id.as_deref() == Some(inspection.workspace.id.as_str())
        });
        let (leftover_count, leftovers) = self.leftovers.of(&inspection.workspace.id);
        let sync = self.checkout_sync(&inspection.workspace.repository_id);
        // A worktree Git cannot read shows none, as `diff_error` says why.
        let changed = self.current_changed_on_both_sides(&inspection.workspace);
        let changed_on_both_sides = self
            .check_access(&path, changed)
            .await
            .ok()
            .flatten()
            .map(Box::new);
        Ok(WorkspaceStatus {
            inspection,
            setup_finished,
            diff,
            diff_error,
            unread_notifications,
            cleanup_error,
            cleanup_reason,
            cleanup_at,
            prs,
            issue_status,
            turn_retry,
            proxy_retry,
            leftover_count,
            leftovers,
            sync,
            changed_on_both_sides,
        })
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

/// `remote` is the workspace's origin remote, as read from its worktree.
async fn issue_status(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    inspection: &Inspection,
) -> Option<IssueStatus> {
    let url = &inspection.issue.as_ref()?.url;
    let closed = async {
        let remote = pr::origin(remote, "issue lookup needs an origin remote")?;
        let forge = ForgeRepo::resolve(remote, hosts).await?;
        let (number, _) = forge.issue(url)?;
        forge.issue_closed(account, number).await
    }
    .await;
    Some(match closed {
        Ok(closed) => IssueStatus {
            state: Some(if closed { "closed" } else { "open" }.into()),
            error: None,
        },
        Err(error) => IssueStatus {
            state: None,
            error: Some(format!("{error:#}")),
        },
    })
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
