//! Persisted opt-in PR watches and manual merge acknowledgements.
pub mod wait;
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{notifications::NotificationKind, store, workspace::Manager},
    forge::{ForgeRepo, repository},
    git,
    model::Workspace,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RegistrationRecord", into = "RegistrationRecord")]
pub struct Registration {
    pub kind: RegistrationKind,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationKind {
    Watch {
        urls: Vec<String>,
        merged_head: Option<String>,
    },
    /// Manual acknowledgement is tied to exactly this commit.
    Acknowledgement { head: String },
}

/// Preserve legacy single-watch records while extending watch sets and completion.
#[derive(Serialize, Deserialize)]
struct RegistrationRecord {
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    urls: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged_head: Option<String>,
    head: Option<String>,
    error: Option<String>,
}

impl TryFrom<RegistrationRecord> for Registration {
    type Error = anyhow::Error;

    fn try_from(record: RegistrationRecord) -> Result<Self> {
        let kind = match (record.url, record.urls, record.head, record.merged_head) {
            (Some(url), None, None, merged_head) => RegistrationKind::Watch {
                urls: vec![url],
                merged_head,
            },
            (None, Some(urls), None, merged_head) => {
                ensure!(
                    !urls.is_empty()
                        && urls.iter().all(|url| !url.is_empty())
                        && urls.iter().collect::<std::collections::HashSet<_>>().len()
                            == urls.len(),
                    "invalid PR watches: expected nonempty, unique URLs"
                );
                RegistrationKind::Watch { urls, merged_head }
            }
            (None, None, Some(head), None) => RegistrationKind::Acknowledgement { head },
            _ => anyhow::bail!(
                "invalid PR cleanup registration: expected exactly one of url or head, or a watch URL list"
            ),
        };
        Ok(Self {
            kind,
            error: record.error,
        })
    }
}

impl From<Registration> for RegistrationRecord {
    fn from(registration: Registration) -> Self {
        let mut record = Self {
            url: None,
            urls: None,
            head: None,
            merged_head: None,
            error: registration.error,
        };
        match registration.kind {
            RegistrationKind::Watch {
                mut urls,
                merged_head,
            } => {
                if urls.len() == 1 {
                    record.url = urls.pop();
                } else {
                    record.urls = Some(urls);
                }
                record.merged_head = merged_head;
            }
            RegistrationKind::Acknowledgement { head } => record.head = Some(head),
        }
        record
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ActionFields", into = "ActionFields")]
pub enum Action {
    Watch { url: String },
    Unwatch { url: String },
    Acknowledge,
    Clear,
}

/// The existing SetPr fields remain flattened on the wire.
#[derive(Serialize, Deserialize)]
struct ActionFields {
    url: Option<String>,
    clear: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unwatch: Option<String>,
}

impl TryFrom<ActionFields> for Action {
    type Error = anyhow::Error;

    fn try_from(fields: ActionFields) -> Result<Self> {
        match (fields.url, fields.clear, fields.unwatch) {
            (Some(url), false, None) => Ok(Self::Watch { url }),
            (None, false, None) => Ok(Self::Acknowledge),
            (None, false, Some(url)) => Ok(Self::Unwatch { url }),
            (None, true, None) => Ok(Self::Clear),
            (Some(_), true, None) => {
                anyhow::bail!("invalid PR cleanup action: clear cannot include a URL")
            }
            _ => anyhow::bail!(
                "invalid PR cleanup action: unwatch cannot be combined with another action"
            ),
        }
    }
}

impl From<Action> for ActionFields {
    fn from(action: Action) -> Self {
        let (url, clear, unwatch) = match action {
            Action::Watch { url } => (Some(url), false, None),
            Action::Unwatch { url } => (None, false, Some(url)),
            Action::Acknowledge => (None, false, None),
            Action::Clear => (None, true, None),
        };
        Self {
            url,
            clear,
            unwatch,
        }
    }
}

impl Manager {
    pub async fn pr_registration(&self, id: &str) -> Result<Option<Registration>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let record: Option<String> = db
                    .query_row(
                        "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?;
                record
                    .map(|s| serde_json::from_str(&s).map_err(Into::into))
                    .transpose()
            })
            .await
    }

    pub async fn set_pr(&self, selector: &str, action: Action) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        ensure!(
            matches!(action, Action::Clear | Action::Unwatch { .. })
                || self
                    .workspace_settings(&workspace)
                    .await?
                    .pr_cleanup
                    .enabled,
            "PR cleanup is disabled by [pr_cleanup] enabled = false"
        );
        if !matches!(action, Action::Clear | Action::Unwatch { .. }) {
            self.verify_worktree(&workspace).await?;
        }
        let kind = match action {
            Action::Clear => None,
            Action::Unwatch { url } => self.without_pr_watch(&workspace, &url).await?,
            Action::Watch { url: input } => {
                current_head(&workspace).await?;
                let (forge, number, url) = self.pr_forge(&workspace, &input).await?;
                // Detect missing tools/login, wrong branches and invalid PRs now.
                forge
                    .merged_commits(&workspace.path, number, &workspace.branch)
                    .await?;
                let (mut urls, mut merged_head) = match self.pr_registration(&workspace.id).await? {
                    Some(Registration {
                        kind: RegistrationKind::Watch { urls, merged_head },
                        ..
                    }) => (urls, merged_head),
                    _ => (Vec::new(), None),
                };
                if !urls.contains(&url) {
                    urls.push(url);
                    merged_head = None;
                }
                Some(RegistrationKind::Watch { urls, merged_head })
            }
            Action::Acknowledge => Some(RegistrationKind::Acknowledgement {
                head: current_head(&workspace).await?,
            }),
        };
        let registration = kind.map(|kind| Registration { kind, error: None });
        let watched = match &registration {
            Some(Registration {
                kind: RegistrationKind::Watch { urls, .. },
                ..
            }) => urls.clone(),
            _ => Vec::new(),
        };
        let id = workspace.id;
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            tx.execute("DELETE FROM pr_activity WHERE workspace_id=?1 AND url NOT IN (SELECT value FROM json_each(?2))", rusqlite::params![id, serde_json::to_string(&watched)?])?;
            if let Some(registration) = registration {
                tx.execute("INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2) ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record", rusqlite::params![id, serde_json::to_string(&registration)?])?;
            } else { tx.execute("DELETE FROM pr_cleanup WHERE workspace_id=?1", [&id])?; }
            tx.commit()?;
            Ok(())
        }).await?;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    async fn without_pr_watch(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<Option<RegistrationKind>> {
        let (_, _, url) = self.pr_forge(workspace, input).await?;
        let Some(Registration {
            kind:
                RegistrationKind::Watch {
                    mut urls,
                    merged_head,
                },
            ..
        }) = self.pr_registration(&workspace.id).await?
        else {
            anyhow::bail!("PR is not watched: {url}");
        };
        ensure!(urls.contains(&url), "PR is not watched: {url}");
        urls.retain(|watched| watched != &url);
        Ok((!urls.is_empty()).then_some(RegistrationKind::Watch { urls, merged_head }))
    }

    async fn pr_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = repository::remote_url_from_path(&workspace.path)
            .await?
            .context("PR lookup needs an origin remote")?;
        let forge = ForgeRepo::parse(&remote)?;
        let (number, url) = forge.pull(input)?;
        Ok((forge, number, url))
    }

    /// Watches survive restarts. A failed lookup never counts as a merge and a
    /// failed removal keeps its registration and ownership records for retry.
    pub async fn sweep_prs(&self) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        for workspace in self.list_workspaces().await? {
            if workspace.state != crate::state::WorkspaceState::Ready
                || self.manual_completion(&workspace.id).await?
            {
                continue;
            }
            let mut registration = match self.pr_registration(&workspace.id).await {
                Ok(Some(registration)) => registration,
                Ok(None) => continue,
                Err(error) => {
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::CleanupFailed,
                        format!("PR cleanup retained the workspace: {error:#}"),
                    )
                    .await;
                    continue;
                }
            };
            // A repository that disabled PR cleanup keeps its watches waiting;
            // unreadable config is recorded like a failed lookup.
            let settings = self.workspace_settings(&workspace).await;
            if settings
                .as_ref()
                .is_ok_and(|settings| !settings.pr_cleanup.enabled)
            {
                continue;
            }
            // `Ok(true)` once the workspace is removed; `Ok(false)` while the PR is open.
            let result: Result<bool> = async {
                settings?;
                self.verify_worktree(&workspace).await?;
                let head = current_head(&workspace).await?;
                if !self
                    .confirm_pr_completion(&workspace, &mut registration.kind, &head)
                    .await?
                {
                    return Ok(false);
                }
                if self.has_holds(&workspace.id).await?
                    || !self.completion_allows_cleanup(&workspace).await?
                {
                    return Ok(false);
                }
                self.remove_merged(&workspace.id, &head).await?;
                eprintln!("PR cleanup removed {}", workspace.name);
                Ok(true)
            }
            .await;
            match &result {
                Ok(true) => {
                    let cause = match registration.kind {
                        RegistrationKind::Watch { .. } => {
                            "removed after all watched pull requests merged"
                        }
                        RegistrationKind::Acknowledgement { .. } => {
                            "removed after the merge acknowledgement"
                        }
                    };
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::WorkspaceRemoved,
                        cause,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(error) => {
                    self.notify(
                        Some(&workspace.name),
                        NotificationKind::CleanupFailed,
                        format!("PR cleanup retained the workspace: {error:#}"),
                    )
                    .await;
                }
            }
            registration.error = result.err().map(|error| format!("{error:#}"));
            let id = workspace.id;
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE pr_cleanup SET record=?2 WHERE workspace_id=?1",
                        rusqlite::params![id, serde_json::to_string(&registration)?],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    async fn confirm_pr_completion(
        &self,
        workspace: &Workspace,
        kind: &mut RegistrationKind,
        head: &str,
    ) -> Result<bool> {
        match kind {
            RegistrationKind::Acknowledgement { head: acknowledged } => {
                ensure!(
                    acknowledged == head,
                    "HEAD changed after the merge acknowledgement; retaining workspace"
                );
            }
            RegistrationKind::Watch { urls, merged_head } => {
                if let Some(confirmed) = merged_head {
                    ensure!(
                        confirmed == head,
                        "HEAD changed after the watched PRs completed; retaining workspace"
                    );
                    return Ok(true);
                }
                if !self.all_prs_merged(workspace, urls, head).await? {
                    return Ok(false);
                }
                // A manual done choice takes precedence over automatic completion.
                if let Some(completion) = self.completion(&workspace.id).await? {
                    ensure!(
                        completion.head == head,
                        "HEAD changed after the assignment was marked done; retaining workspace"
                    );
                } else {
                    self.record_done(workspace, head.to_owned(), None).await?;
                }
                // Completion is persisted first; after a crash it is reused above.
                // The latch prevents repeated forge queries and completion signals.
                *merged_head = Some(head.to_owned());
            }
        }
        Ok(true)
    }

    async fn all_prs_merged(
        &self,
        workspace: &Workspace,
        urls: &[String],
        head: &str,
    ) -> Result<bool> {
        let mut contains_head = false;
        for url in urls {
            let (forge, number, _) = self.pr_forge(workspace, url).await?;
            let Some(commits) = forge
                .merged_commits(&workspace.path, number, &workspace.branch)
                .await?
            else {
                return Ok(false);
            };
            contains_head |= commits.iter().any(|commit| commit == head);
        }
        ensure!(
            contains_head,
            "merged PR set does not contain the current workspace commit; retaining workspace"
        );
        Ok(true)
    }
}

pub(crate) async fn current_head(workspace: &Workspace) -> Result<String> {
    ensure!(
        git::run(&workspace.path, &["branch", "--show-current"])
            .await?
            .trim_end()
            == workspace.branch,
        "workspace is not on its recorded branch"
    );
    Ok(git::run(&workspace.path, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn unwatch_removes_only_the_selected_pr_even_when_cleanup_is_disabled() {
        use crate::test_support::{git, manager, repository};
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "watch".into(), None, None, None)
            .await
            .unwrap();
        git(
            &path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/team/repo.git",
            ],
        );
        manager
            .set_repository_config(&repo.id, Some("[pr_cleanup]\nenabled=false\n".into()))
            .await
            .unwrap();
        let id = workspace.id.clone();
        manager.store.run(move |db| {
            db.execute("INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                rusqlite::params![id, json!({"urls":["https://github.com/team/repo/pull/1", "https://github.com/team/repo/pull/2"]}).to_string()])?;
            Ok(())
        }).await.unwrap();
        assert!(
            manager
                .set_pr(&workspace.id, Action::Unwatch { url: "3".into() })
                .await
                .is_err()
        );
        manager
            .set_pr(&workspace.id, Action::Unwatch { url: "1".into() })
            .await
            .unwrap();
        assert!(
            matches!(manager.pr_registration(&workspace.id).await.unwrap().unwrap().kind,
            RegistrationKind::Watch { urls, .. } if urls == ["https://github.com/team/repo/pull/2"])
        );
        manager
            .set_pr(&workspace.id, Action::Unwatch { url: "2".into() })
            .await
            .unwrap();
        assert!(
            manager
                .pr_registration(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(manager.completion(&workspace.id).await.unwrap().is_none());
        assert!(workspace.path.exists());
    }

    #[test]
    fn registration_round_trips_legacy_records() {
        for (kind, url, head) in [
            (
                RegistrationKind::Watch {
                    urls: vec!["https://forge.example/team/repo/pulls/7".into()],
                    merged_head: None,
                },
                Some("https://forge.example/team/repo/pulls/7"),
                None,
            ),
            (
                RegistrationKind::Acknowledgement {
                    head: "abc123".into(),
                },
                None,
                Some("abc123"),
            ),
        ] {
            for error in [None, Some("lookup failed")] {
                let record = json!({"url": url, "head": head, "error": error});
                let registration: Registration = serde_json::from_value(record.clone()).unwrap();
                assert_eq!(registration.kind, kind);
                assert_eq!(registration.error.as_deref(), error);
                assert_eq!(serde_json::to_value(&registration).unwrap(), record);
            }
        }
        // Optional fields were also allowed to be absent.
        let registration: Registration = serde_json::from_value(json!({"head": "abc123"})).unwrap();
        assert_eq!(
            serde_json::to_value(registration).unwrap(),
            json!({"url": null, "head": "abc123", "error": null})
        );
    }

    #[test]
    fn multiple_watches_round_trip_and_reject_ambiguous_or_empty_sets() {
        let value = json!({"url": null, "urls": ["first", "second"], "head": null,
            "merged_head": "commit", "error": null});
        let registration: Registration = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(registration).unwrap(), value);
        for value in [
            json!({"urls": []}),
            json!({"urls": ["same", "same"]}),
            json!({"urls": [""]}),
            json!({"url": "first", "urls": ["second"]}),
            json!({"urls": ["first"], "head": "commit"}),
            json!({"head": "commit", "merged_head": "commit"}),
        ] {
            assert!(serde_json::from_value::<Registration>(value).is_err());
        }
    }

    #[test]
    fn registration_rejects_ambiguous_records() {
        for record in [
            json!({}),
            json!({"url": null, "head": null}),
            json!({"url": "url", "head": "head"}),
        ] {
            let error = serde_json::from_value::<Registration>(record).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("expected exactly one of url or head"),
                "{error}"
            );
        }
    }
}
