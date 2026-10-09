use anyhow::{Context, Result, bail, ensure};

use super::Manager;
use crate::{
    daemon::store,
    forge::{
        ForgeRepo, IssueInput,
        link::{self, ItemKind},
        pr::{self, wait::linked_items},
        repository,
        view::ItemView,
    },
    git,
    model::{DiffSummary, Workspace, WorkspaceStatus, WorkspaceTarget},
};

impl Manager {
    pub async fn workspace_status(&self, selector: &str) -> Result<WorkspaceStatus> {
        let mut inspection = self.inspect_workspace(selector).await?;
        let diff = async {
            self.verify_worktree(&inspection.workspace).await?;
            let base = self.diff_base(&inspection.workspace.id).await?;
            let numstat = git::run(
                &inspection.workspace.path,
                &["diff", "--numstat", &base.commit, "--"],
            )
            .await?;
            parse_numstat(&numstat)
        }
        .await;
        let (diff, diff_error) = match diff {
            Ok(diff) => (Some(diff), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let prs = pr::state::watched(&inspection.workspace, inspection.pr_cleanup.as_ref()).await;
        let unread_notifications = self.unread_notifications().await?;
        let workspace_id = inspection.workspace.id.clone();
        let setup_finished = self
            .store
            .run(move |db| store::setup_finished(db, &workspace_id))
            .await?;
        inspection.simulators.retain(|simulator| {
            simulator.workspace_id.as_deref() == Some(inspection.workspace.id.as_str())
        });
        Ok(WorkspaceStatus {
            inspection,
            setup_finished,
            diff,
            diff_error,
            unread_notifications,
            cleanup_error: self.cleanup_problem(),
            prs,
        })
    }
}

impl Manager {
    /// Each selected item as the forge reports it now, in link order.
    pub async fn view_items(
        &self,
        selector: &str,
        selection: &link::Selection,
        comments: bool,
    ) -> Result<Vec<ItemView>> {
        let workspace = self.workspace(selector).await?;
        let items = self.selected_items(&workspace, selection).await?;
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("item lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        Ok(futures_util::future::join_all(
            items
                .iter()
                .map(|(url, kind)| forge.view(&workspace.path, *kind, url, comments)),
        )
        .await)
    }

    /// Workspaces that link an issue or PR, or hold a lease on a resource,
    /// optionally only among the one a scoped caller owns.
    pub async fn find_workspaces(
        &self,
        target: &WorkspaceTarget,
        within: Option<&str>,
    ) -> Result<Vec<Workspace>> {
        let within = within.map(str::to_owned);
        let ids = match target {
            WorkspaceTarget::Item { kind, input } => {
                self.linking_workspaces(*kind, input, within).await?
            }
            WorkspaceTarget::Resource { name } => self.holding_workspaces(name, within).await?,
        };
        let mut workspaces = self.list_workspaces().await?;
        workspaces.retain(|workspace| ids.contains(&workspace.id));
        Ok(workspaces)
    }

    async fn linking_workspaces(
        &self,
        kind: ItemKind,
        input: &str,
        within: Option<String>,
    ) -> Result<Vec<String>> {
        let label = match kind {
            ItemKind::Pr => "PR",
            ItemKind::Issue => "issue",
        };
        let wanted = match IssueInput::parse(input) {
            IssueInput::Url => Some(link::item(kind, input)?),
            IssueInput::Number => None,
            IssueInput::Invalid => bail!("{label} must be a number or URL"),
        };
        let number = match &wanted {
            Some((_, number)) => *number,
            None => input
                .parse::<u64>()
                .ok()
                .filter(|number| *number > 0)
                .with_context(|| format!("invalid {label} number"))?,
        };
        let links = self
            .store
            .run(move |db| {
                let ids = db
                    .prepare("SELECT id FROM workspaces WHERE ?1 IS NULL OR id=?1")?
                    .query_map([within], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut links = Vec::new();
                for id in ids {
                    for (url, _) in linked_items(db, &id, Some(kind))? {
                        links.push((id.clone(), url));
                    }
                }
                Ok(links)
            })
            .await?;
        let mut matches: Vec<(String, ForgeRepo)> = Vec::new();
        for (id, url) in links {
            let Ok((repository, linked)) = link::item(kind, &url) else {
                continue;
            };
            let found = linked == number
                && wanted
                    .as_ref()
                    .is_none_or(|(wanted, _)| *wanted == repository);
            if found && !matches.iter().any(|(matched, _)| *matched == id) {
                matches.push((id, repository));
            }
        }
        ensure!(!matches.is_empty(), "no workspace links {label} {input}");
        ensure!(
            matches
                .iter()
                .all(|(_, repository)| *repository == matches[0].1),
            "{label} {input} is linked in several repositories; pass its URL"
        );
        Ok(matches.into_iter().map(|(id, _)| id).collect())
    }

    async fn holding_workspaces(&self, name: &str, within: Option<String>) -> Result<Vec<String>> {
        let name = name.to_owned();
        let label = name.clone();
        let ids = self
            .store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT workspace_id FROM resource_leases
                         WHERE (pool=?1 OR resource=?1) AND (?2 IS NULL OR workspace_id=?2)",
                    )?
                    .query_map(rusqlite::params![name, within], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        ensure!(!ids.is_empty(), "no workspace holds resource {label}");
        Ok(ids)
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
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_text_and_binary_files() {
        let summary = parse_numstat("2\t1\tfirst\n10\t0\tsecond\n-\t-\timage.png\n").unwrap();
        assert_eq!(summary.files_changed, 3);
        assert_eq!(summary.insertions, 12);
        assert_eq!(summary.deletions, 1);
    }

    #[test]
    fn numstat_rejects_incomplete_records() {
        assert!(parse_numstat("1\t2\n").is_err());
    }
}
