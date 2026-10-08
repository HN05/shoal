//! Persist issue associations and complete assignments when their issues close.
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::{ForgeKind, ForgeRepo, Query, forgejo_details, repository};
use crate::{
    daemon::{notifications::NotificationKind, workspace::Manager},
    model::Workspace,
    state::WorkspaceState,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub url: String,
    pub error: Option<String>,
}

impl Manager {
    pub async fn issue_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT url,error FROM workspace_issue WHERE workspace_id=?1",
                        [id],
                        |row| {
                            Ok(Registration {
                                url: row.get(0)?,
                                error: row.get(1)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// Association is part of opening a workspace, before tracked setup starts.
    pub async fn set_issue(&self, selector: &str, input: &str) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (_, _, url) = self.issue_forge(&workspace, input).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let state: WorkspaceState = tx.query_row(
                    "SELECT state FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |row| row.get(0),
                )?;
                ensure!(
                    matches!(
                        state,
                        WorkspaceState::Ready | WorkspaceState::Preparing | WorkspaceState::Failed
                    ),
                    "workspace cannot accept an issue in its current state"
                );
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT url FROM workspace_issue WHERE workspace_id=?1",
                        [&workspace.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    existing.as_ref().is_none_or(|existing| existing == &url),
                    "workspace is already associated with a different issue"
                );
                tx.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,?2)
                 ON CONFLICT(workspace_id) DO NOTHING",
                    rusqlite::params![workspace.id, url],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn clear_issue(&self, selector: &str, input: Option<&str>) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        let selected = match input {
            Some(input) => Some(self.issue_forge(&workspace, input).await?.2),
            None => None,
        };
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT url FROM workspace_issue WHERE workspace_id=?1",
                        [&workspace.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(url) = selected {
                    ensure!(
                        existing.as_ref() == Some(&url),
                        "issue is not linked: {url}"
                    );
                }
                if let Some(url) = existing {
                    tx.execute(
                        "DELETE FROM pr_activity WHERE workspace_id=?1 AND url=?2",
                        rusqlite::params![workspace.id, url],
                    )?;
                }
                tx.execute(
                    "DELETE FROM workspace_issue WHERE workspace_id=?1",
                    [&workspace.id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    /// Completion is a one-shot signal: never overwrite a prior keep choice or
    /// bind an old closure to later work, including after daemon restart.
    pub async fn sweep_issues(&self) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        for workspace in self.list_workspaces().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            let result = self.complete_closed_issue(&workspace).await;
            if let Err(error) = &result {
                self.record_retained(
                    &workspace.id,
                    crate::daemon::events::EventCause::Issue,
                    error,
                )
                .await?;
                self.notify(
                    Some(&workspace.name),
                    NotificationKind::CleanupFailed,
                    format!("issue completion retained the workspace: {error:#}"),
                )
                .await;
            }
            let id = workspace.id;
            let error = result.err().map(|error| format!("{error:#}"));
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE workspace_issue SET error=?2 WHERE workspace_id=?1",
                        rusqlite::params![id, error],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    async fn complete_closed_issue(&self, workspace: &Workspace) -> Result<()> {
        let Some(issue) = self.issue_registration(&workspace.id).await? else {
            return Ok(());
        };
        if self.manual_completion(&workspace.id).await?
            || self.completion(&workspace.id).await?.is_some()
            || !self.workspace_settings(workspace).await?.done.automatic
        {
            return Ok(());
        }
        self.verify_worktree(workspace).await?;
        let head = super::pr::current_head(workspace).await?;
        let (forge, number, _) = self.issue_forge(workspace, &issue.url).await?;
        if forge.issue_closed(&workspace.path, number).await? {
            self.record_done(
                workspace,
                head,
                None,
                crate::daemon::events::EventCause::Issue,
            )
            .await?;
        }
        Ok(())
    }

    async fn issue_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let (number, url) = forge.issue(input)?;
        Ok((forge, number, url))
    }
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum IssueState {
    Open,
    Closed,
}

impl ForgeRepo {
    pub async fn issue_closed(&self, path: &std::path::Path, number: u64) -> Result<bool> {
        let id = number.to_string();
        let repo = format!("{}/{}", self.host, self.path);
        let args = match self.kind {
            ForgeKind::GitHub => vec![
                "issue",
                "view",
                &id,
                "--repo",
                &repo,
                "--json",
                "number,state",
            ],
            ForgeKind::Forgejo => vec![
                "--style", "minimal", "issue", "view", &id, "--host", &self.host, "--remote",
                "origin",
            ],
        };
        let output = self.kind.query(path, &args, Query::Issue).await?;
        Ok(parse_state(self.kind, &output, number)? == IssueState::Closed)
    }
}

pub(super) fn require_open(kind: ForgeKind, output: &str, number: u64) -> Result<()> {
    ensure!(
        parse_state(kind, output, number)? == IssueState::Open,
        "issue is already closed; reopen it before starting an issue workspace"
    );
    Ok(())
}

fn parse_state(kind: ForgeKind, output: &str, number: u64) -> Result<IssueState> {
    match kind {
        ForgeKind::GitHub => {
            #[derive(serde::Deserialize)]
            struct Issue {
                number: u64,
                state: IssueState,
            }
            let issue: Issue = serde_json::from_str(output).context("invalid gh issue response")?;
            ensure!(issue.number == number, "gh returned a different issue");
            Ok(issue.state)
        }
        ForgeKind::Forgejo => {
            let (_, details) = forgejo_details(output, number)?;
            let (_, state) = details
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("By "))
                .and_then(|line| line.rsplit_once(" — "))
                .context("unrecognized fj issue state")?;
            match state {
                "Open" => Ok(IssueState::Open),
                "Closed" => Ok(IssueState::Closed),
                _ => anyhow::bail!("unknown fj issue state: {state}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, manager, repository};

    #[tokio::test]
    async fn association_is_persistent_idempotent_and_bound_to_the_origin() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        git(&path, &["config", "protocol.allow", "never"]);
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "issue".into(), Some("HEAD".into()), None, None)
            .await
            .unwrap();
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        let url = "https://github.com/team/repo/issues/316";
        manager.set_issue(&workspace.id, url).await.unwrap();
        manager.set_issue(&workspace.id, "316").await.unwrap();
        assert!(manager.set_issue(&workspace.id, "317").await.is_err());
        assert!(
            manager
                .set_issue(&workspace.id, "https://github.com/other/repo/issues/316")
                .await
                .is_err()
        );
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        let reopened = Manager::open(manager.paths.clone()).await.unwrap();
        assert_eq!(
            reopened
                .inspect_workspace(&workspace.id)
                .await
                .unwrap()
                .issue
                .unwrap()
                .url,
            url
        );
        assert!(
            reopened
                .clear_issue(&workspace.id, Some("317"))
                .await
                .is_err()
        );
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .is_some()
        );
        reopened
            .clear_issue(&workspace.id, Some("316"))
            .await
            .unwrap();
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        reopened.set_issue(&workspace.id, "317").await.unwrap();
        assert!(
            reopened
                .issue_registration(&workspace.id)
                .await
                .unwrap()
                .unwrap()
                .url
                .ends_with("/317")
        );
        reopened.store.shutdown().await;
    }

    #[test]
    fn github_requires_matching_number_and_known_state() {
        for (state, expected) in [("OPEN", IssueState::Open), ("CLOSED", IssueState::Closed)] {
            let output = serde_json::json!({"number": 316, "state": state}).to_string();
            assert_eq!(
                parse_state(ForgeKind::GitHub, &output, 316).unwrap(),
                expected
            );
            assert!(parse_state(ForgeKind::GitHub, &output, 1).is_err());
        }
        for output in [
            r#"{"number":316,"state":"MERGED"}"#,
            r#"{"number":316}"#,
            "",
        ] {
            assert!(parse_state(ForgeKind::GitHub, output, 316).is_err());
        }
    }

    #[test]
    fn forgejo_checks_header_state_without_reading_body_as_status() {
        for (state, expected) in [("Open", IssueState::Open), ("Closed", IssueState::Closed)] {
            let output = format!(
                "\u{2068}Issue title\u{2069} #\u{2068}316\u{2069}\"\nBy user — \u{2068}{state}\u{2069}\n\nBy user — Closed\n\n0 comments\n"
            );
            assert_eq!(
                parse_state(ForgeKind::Forgejo, &output, 316).unwrap(),
                expected
            );
            assert!(parse_state(ForgeKind::Forgejo, &output, 1).is_err());
        }
        for details in [
            "By user — Merged",
            "By user",
            "garbage\nBy user — Closed",
            "",
        ] {
            let output = format!("Issue title #316\n{details}");
            assert!(parse_state(ForgeKind::Forgejo, &output, 316).is_err());
        }
    }
}
