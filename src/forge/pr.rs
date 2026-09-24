//! Persisted opt-in PR watches and manual merge acknowledgements.
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
        url: String,
    },
    /// Manual acknowledgement is tied to exactly this commit.
    Acknowledgement {
        head: String,
    },
}

/// Keep the persisted record and CLI JSON compatible with existing daemons.
#[derive(Serialize, Deserialize)]
struct RegistrationRecord {
    url: Option<String>,
    head: Option<String>,
    error: Option<String>,
}

impl TryFrom<RegistrationRecord> for Registration {
    type Error = anyhow::Error;

    fn try_from(record: RegistrationRecord) -> Result<Self> {
        let kind = match (record.url, record.head) {
            (Some(url), None) => RegistrationKind::Watch { url },
            (None, Some(head)) => RegistrationKind::Acknowledgement { head },
            _ => anyhow::bail!(
                "invalid PR cleanup registration: expected exactly one of url or head"
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
        let (url, head) = match registration.kind {
            RegistrationKind::Watch { url } => (Some(url), None),
            RegistrationKind::Acknowledgement { head } => (None, Some(head)),
        };
        Self {
            url,
            head,
            error: registration.error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ActionFields", into = "ActionFields")]
pub enum Action {
    Watch { url: String },
    Acknowledge,
    Clear,
}

/// The existing SetPr fields remain flattened on the wire.
#[derive(Serialize, Deserialize)]
struct ActionFields {
    url: Option<String>,
    clear: bool,
}

impl TryFrom<ActionFields> for Action {
    type Error = anyhow::Error;

    fn try_from(fields: ActionFields) -> Result<Self> {
        match (fields.url, fields.clear) {
            (Some(url), false) => Ok(Self::Watch { url }),
            (None, false) => Ok(Self::Acknowledge),
            (None, true) => Ok(Self::Clear),
            (Some(_), true) => {
                anyhow::bail!("invalid PR cleanup action: clear cannot include a URL")
            }
        }
    }
}

impl From<Action> for ActionFields {
    fn from(action: Action) -> Self {
        let (url, clear) = match action {
            Action::Watch { url } => (Some(url), false),
            Action::Acknowledge => (None, false),
            Action::Clear => (None, true),
        };
        Self { url, clear }
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
            matches!(action, Action::Clear)
                || self
                    .workspace_settings(&workspace)
                    .await?
                    .pr_cleanup
                    .enabled,
            "PR cleanup is disabled by [pr_cleanup] enabled = false"
        );
        if !matches!(action, Action::Clear) {
            self.verify_worktree(&workspace).await?;
        }
        let kind = match action {
            Action::Clear => None,
            Action::Watch { url: input } => {
                current_head(&workspace).await?;
                let (forge, number, url) = self.pr_forge(&workspace, &input).await?;
                // Detect missing tools/login, wrong branches and invalid PRs now.
                forge
                    .merged_commits(&workspace.path, number, &workspace.branch)
                    .await?;
                Some(RegistrationKind::Watch { url })
            }
            Action::Acknowledge => Some(RegistrationKind::Acknowledgement {
                head: current_head(&workspace).await?,
            }),
        };
        let registration = kind.map(|kind| Registration { kind, error: None });
        let id = workspace.id;
        self.store.run(move |db| {
            let tx = db.transaction()?;
            store::require_ready(&tx, &id)?;
            if let Some(registration) = registration {
                tx.execute("INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2) ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record", rusqlite::params![id, serde_json::to_string(&registration)?])?;
            } else { tx.execute("DELETE FROM pr_cleanup WHERE workspace_id=?1", [&id])?; }
            tx.commit()?;
            Ok(())
        }).await?;
        self.cleanup_notify.notify_one();
        Ok(())
    }

    async fn pr_forge(
        &self,
        workspace: &Workspace,
        input: &str,
    ) -> Result<(ForgeRepo, u64, String)> {
        let remote = repository::remote_url(
            workspace
                .path
                .to_str()
                .context("workspace path is not UTF-8")?,
        )
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
            if workspace.state != crate::state::WorkspaceState::Ready {
                continue;
            }
            let Some(mut registration) = self.pr_registration(&workspace.id).await? else {
                continue;
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
                match &registration.kind {
                    RegistrationKind::Watch { url } => {
                        let (forge, number, _) = self.pr_forge(&workspace, url).await?;
                        let Some(commits) = forge.merged_commits(&workspace.path, number, &workspace.branch).await? else { return Ok(false); };
                        ensure!(commits.contains(&head), "merged PR does not contain the current workspace commit; retaining workspace");
                    }
                    RegistrationKind::Acknowledgement { head: acknowledged } => {
                        ensure!(acknowledged == &head, "HEAD changed after the merge acknowledgement; retaining workspace");
                    }
                }
                self.remove_merged(&workspace.id, &head).await?;
                eprintln!("PR cleanup removed {}", workspace.name);
                Ok(true)
            }.await;
            match &result {
                Ok(true) => {
                    let cause = match registration.kind {
                        RegistrationKind::Watch { .. } => "removed after its pull request merged",
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

    #[test]
    fn registration_round_trips_legacy_records() {
        for (kind, url, head) in [
            (
                RegistrationKind::Watch {
                    url: "https://forge.example/team/repo/pulls/7".into(),
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
