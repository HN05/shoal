//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the workspace's agent account.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
    item::ItemState,
    items::{linked, workspace_forge},
    link::ItemKind,
};
use crate::{
    daemon::{events, workspace::Manager},
    state::states,
};

states!(CiAction: ValueEnum {
    Rerun => "rerun",
    Cancel => "cancel",
});

/// The checks an action reached on a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiActed {
    pub url: String,
    pub action: CiAction,
    pub checks: Vec<String>,
}

/// A CI job on a PR's head under the check name the forge shows, with the web
/// URL that locates it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    name: String,
    state: JobState,
    target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Failed,
    Running,
    Finished,
}

impl CiAction {
    /// The state of the jobs the action applies to.
    fn applies_to(self) -> (JobState, &'static str) {
        match self {
            Self::Rerun => (JobState::Failed, "failed"),
            Self::Cancel => (JobState::Running, "running"),
        }
    }
}

impl Manager {
    /// Rerun the failed, or cancel the running, `checks` of a PR in the
    /// workspace's repository, or every such check when none is named.
    pub async fn ci_action(
        &self,
        selector: &str,
        item: Option<String>,
        action: CiAction,
        checks: Vec<String>,
    ) -> Result<CiActed> {
        for check in &checks {
            ensure!(
                !check.trim().is_empty() && check.len() <= 1024,
                "check name must be nonempty and at most 1024 bytes"
            );
        }
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = workspace_forge(&workspace).await?;
        let input = match item {
            Some(input) => input,
            None => linked(&workspace, ItemKind::Pr)?,
        };
        let (number, url) = forge.pull(&input)?;
        let account = self.agent_account(&workspace).await?;
        let checks = forge
            .ci_action(&account, number, action, &checks)
            .await
            .with_context(|| format!("could not {action} checks of {url}"))?;
        let (id, journaled) = (workspace.id.clone(), url.clone());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Pr, &journaled, action.as_str()))
            .await?;
        Ok(CiActed {
            url,
            action,
            checks,
        })
    }
}

impl ForgeRepo {
    /// Apply `action` to the named checks on the open PR's head, or to every
    /// check it applies to, and return the names of the checks it reached.
    async fn ci_action(
        &self,
        account: &Account,
        number: u64,
        action: CiAction,
        names: &[String],
    ) -> Result<Vec<String>> {
        self.supports_ci_action(action)?;
        let pull = self.read(account, ItemKind::Pr, number).await?;
        ensure!(pull.item.state == ItemState::Open, "#{number} is not open");
        let head = pull.head_sha.context("PR has no head commit")?;
        let jobs = self.ci_jobs(account, &head).await?;
        let selected = select(&jobs, action, names)?;
        let mut endpoints = Vec::new();
        for job in &selected {
            let endpoint = self.ci_endpoint(account, &head, action, job).await?;
            // Jobs of one run share its cancellation.
            if !endpoints.contains(&endpoint) {
                endpoints.push(endpoint);
            }
        }
        for endpoint in endpoints {
            let request = Request::new(HttpMethod::Post, self.repo_endpoint(&endpoint), None);
            self.send(account, &request).await?;
        }
        let mut checks = Vec::new();
        for job in selected {
            if !checks.contains(&job.name) {
                checks.push(job.name);
            }
        }
        Ok(checks)
    }

    fn supports_ci_action(&self, action: CiAction) -> Result<()> {
        match (self.kind, action) {
            (ForgeKind::GitHub | ForgeKind::GitLab, _) | (ForgeKind::Forgejo, CiAction::Cancel) => {
                Ok(())
            }
            (ForgeKind::Forgejo, CiAction::Rerun) => bail!(
                "Forgejo's API cannot rerun Actions jobs; rerun them from the web UI or push a fix"
            ),
            (ForgeKind::Bitbucket, _) => bail!("Shoal cannot {action} Bitbucket Pipelines"),
        }
    }

    /// Each CI job's latest attempt on `head`.
    async fn ci_jobs(&self, account: &Account, head: &str) -> Result<Vec<Job>> {
        match self.kind {
            ForgeKind::GitHub => {
                let endpoint =
                    self.repo_endpoint(&format!("commits/{head}/check-runs?per_page=100"));
                github_jobs(&self.send(account, &Request::get(endpoint)).await?)
            }
            ForgeKind::Forgejo => {
                let endpoint = self.repo_endpoint(&format!("commits/{head}/status?limit=50"));
                forgejo_jobs(&self.send(account, &Request::get(endpoint)).await?)
            }
            ForgeKind::GitLab => {
                let endpoint = self.repo_endpoint(&format!("repository/commits/{head}/statuses"));
                gitlab_jobs(&self.pages(account, &endpoint).await?)
            }
            ForgeKind::Bitbucket => bail!("Bitbucket Pipelines are not supported"),
        }
    }

    /// The endpoint, relative to the repository, that applies `action` to `job`.
    async fn ci_endpoint(
        &self,
        account: &Account,
        head: &str,
        action: CiAction,
        job: &Job,
    ) -> Result<String> {
        Ok(match (self.kind, action) {
            (ForgeKind::GitHub, CiAction::Rerun) => {
                let (_, job) = self.github_job(&job.target)?;
                format!("actions/jobs/{job}/rerun")
            }
            (ForgeKind::GitHub, CiAction::Cancel) => {
                let (run, _) = self.github_job(&job.target)?;
                format!("actions/runs/{run}/cancel")
            }
            (ForgeKind::GitLab, CiAction::Rerun) => {
                format!("jobs/{}/retry", self.gitlab_job(&job.target)?)
            }
            (ForgeKind::GitLab, CiAction::Cancel) => {
                format!("jobs/{}/cancel", self.gitlab_job(&job.target)?)
            }
            (ForgeKind::Forgejo, CiAction::Cancel) => {
                let (number, _) = self.job_location(&job.target)?;
                let run = self.forgejo_run_id(account, number, head).await?;
                format!("actions/runs/{run}/cancel")
            }
            (ForgeKind::Forgejo, CiAction::Rerun) | (ForgeKind::Bitbucket, _) => {
                bail!("Shoal cannot {action} CI jobs on this forge")
            }
        })
    }

    /// The run and job IDs of a GitHub Actions job URL in this repository.
    fn github_job(&self, target: &str) -> Result<(u64, u64)> {
        let parts = self.repository_path(target, "/actions/runs/")?;
        let [run, "job", job] = parts.as_slice() else {
            bail!("check does not link to a GitHub Actions job")
        };
        Ok((id(run)?, id(job)?))
    }

    /// The job ID of a GitLab CI job URL in this repository.
    fn gitlab_job(&self, target: &str) -> Result<u64> {
        let parts = self.repository_path(target, "/-/jobs/")?;
        let [job] = parts.as_slice() else {
            bail!("check does not link to a GitLab CI job")
        };
        id(job)
    }

    /// The path components of `target` after this repository's `section`.
    fn repository_path<'a>(&self, target: &'a str, section: &str) -> Result<Vec<&'a str>> {
        Ok(target
            .strip_prefix(&self.repository_url())
            .and_then(|path| path.strip_prefix(section))
            .context("check does not link to this repository's CI job")?
            .split('/')
            .collect())
    }
}

fn id(text: &str) -> Result<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .context("invalid CI job ID")
}

/// The jobs `action` applies to: those of the named checks, at least one per
/// name, or every job it applies to.
fn select(jobs: &[Job], action: CiAction, names: &[String]) -> Result<Vec<Job>> {
    let (state, described) = action.applies_to();
    if names.is_empty() {
        let selected = jobs
            .iter()
            .filter(|job| job.state == state)
            .cloned()
            .collect::<Vec<_>>();
        ensure!(
            !selected.is_empty(),
            "no check on the PR's head is {described}"
        );
        return Ok(selected);
    }
    let mut selected = Vec::new();
    for name in names {
        let named = jobs.iter().filter(|job| job.name == *name);
        ensure!(
            named.clone().next().is_some(),
            "check {name} was not found on the PR's current head"
        );
        let matching = named.filter(|job| job.state == state).collect::<Vec<_>>();
        ensure!(!matching.is_empty(), "check {name} is not {described}");
        selected.extend(matching.into_iter().cloned());
    }
    Ok(selected)
}

/// GitHub's latest check runs; only Actions jobs link to a rerunnable job.
fn github_jobs(response: &Value) -> Result<Vec<Job>> {
    let runs = response["check_runs"]
        .as_array()
        .context("GitHub check runs are not a list")?;
    ensure!(
        response["total_count"].as_u64() == Some(runs.len() as u64),
        "GitHub returned a partial page of check runs"
    );
    runs.iter()
        .map(|run| {
            let state = match (run["status"].as_str(), run["conclusion"].as_str()) {
                (
                    Some("completed"),
                    Some("failure" | "timed_out" | "cancelled" | "startup_failure"),
                ) => JobState::Failed,
                (Some("completed"), _) => JobState::Finished,
                (Some(_), _) => JobState::Running,
                (None, _) => bail!("GitHub check run has no status"),
            };
            job(run, "name", state, "details_url")
        })
        .collect()
}

/// Forgejo's latest status of each check.
fn forgejo_jobs(combined: &Value) -> Result<Vec<Job>> {
    let statuses = match &combined["statuses"] {
        Value::Null => &[][..],
        statuses => statuses.as_array().context("invalid Forgejo CI statuses")?,
    };
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "Forgejo returned a partial page of CI statuses"
    );
    statuses
        .iter()
        .map(|status| {
            let state = match status["status"].as_str() {
                Some("pending") => JobState::Running,
                Some("failure" | "error") => JobState::Failed,
                Some("success" | "warning" | "skipped") => JobState::Finished,
                _ => bail!("unknown Forgejo CI state"),
            };
            job(status, "context", state, "target_url")
        })
        .collect()
}

/// GitLab's newest status of each name; a retried job keeps its earlier ones.
fn gitlab_jobs(statuses: &[Value]) -> Result<Vec<Job>> {
    let mut latest = std::collections::BTreeMap::new();
    for status in statuses {
        let id = status["id"].as_u64().context("GitLab status has no ID")?;
        let state = match status["status"].as_str() {
            Some(
                "created"
                | "waiting_for_resource"
                | "preparing"
                | "pending"
                | "running"
                | "scheduled",
            ) => JobState::Running,
            Some("failed" | "canceled") => JobState::Failed,
            Some("success" | "skipped" | "manual" | "canceling" | "waiting_for_callback") => {
                JobState::Finished
            }
            state => bail!("unknown GitLab CI state {state:?}"),
        };
        let job = job(status, "name", state, "target_url")?;
        if latest
            .get(&job.name)
            .is_none_or(|(newest, _): &(u64, Job)| *newest < id)
        {
            latest.insert(job.name.clone(), (id, job));
        }
    }
    Ok(latest.into_values().map(|(_, job)| job).collect())
}

fn job(value: &Value, name: &str, state: JobState, target: &str) -> Result<Job> {
    Ok(Job {
        name: value[name]
            .as_str()
            .filter(|name| !name.is_empty())
            .context("CI check has no name")?
            .into(),
        state,
        target: value[target].as_str().unwrap_or_default().into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(name: &str, state: JobState) -> Job {
        Job {
            name: name.into(),
            state,
            target: String::new(),
        }
    }

    #[test]
    fn actions_select_named_checks_in_their_state_or_every_one() {
        let jobs = [
            job("rust", JobState::Failed),
            job("docs", JobState::Running),
            job("lint", JobState::Finished),
            job("rust", JobState::Finished),
        ];
        let names = |selected: Vec<Job>| selected.into_iter().map(|j| j.name).collect::<Vec<_>>();
        assert_eq!(
            names(select(&jobs, CiAction::Rerun, &[]).unwrap()),
            ["rust"]
        );
        assert_eq!(
            names(select(&jobs, CiAction::Cancel, &[]).unwrap()),
            ["docs"]
        );
        assert_eq!(
            names(select(&jobs, CiAction::Rerun, &["rust".into()]).unwrap()),
            ["rust"]
        );
        let error = select(&jobs, CiAction::Rerun, &["lint".into()]).unwrap_err();
        assert_eq!(error.to_string(), "check lint is not failed");
        let error = select(&jobs, CiAction::Cancel, &["other".into()]).unwrap_err();
        assert!(error.to_string().contains("was not found"), "{error}");
        let error = select(&jobs[2..], CiAction::Rerun, &[]).unwrap_err();
        assert_eq!(error.to_string(), "no check on the PR's head is failed");
    }

    #[test]
    fn job_urls_must_belong_to_the_repository() {
        let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
        assert_eq!(
            github
                .github_job("https://github.com/team/repo/actions/runs/12/job/34")
                .unwrap(),
            (12, 34)
        );
        for target in [
            "https://github.com/other/repo/actions/runs/12/job/34",
            "https://github.com/team/repo/actions/runs/12",
            "https://github.com/team/repo/actions/runs/0/job/34",
            "https://checks.example/team/repo/actions/runs/12/job/34",
            "",
        ] {
            assert!(github.github_job(target).is_err(), "{target}");
        }
        let gitlab = ForgeRepo::parse("https://gitlab.com/group/sub/repo").unwrap();
        assert_eq!(
            gitlab
                .gitlab_job("https://gitlab.com/group/sub/repo/-/jobs/56")
                .unwrap(),
            56
        );
        for target in [
            "https://gitlab.com/group/repo/-/jobs/56",
            "https://gitlab.com/group/sub/repo/-/pipelines/56",
            "https://gitlab.com/group/sub/repo/-/jobs/56/retry",
        ] {
            assert!(gitlab.gitlab_job(target).is_err(), "{target}");
        }
    }

    #[test]
    fn each_forge_reports_failed_and_running_jobs() {
        let github = json!({"total_count": 3, "check_runs": [
            {"name": "rust", "status": "completed", "conclusion": "timed_out", "details_url": "u"},
            {"name": "docs", "status": "queued", "conclusion": null},
            {"name": "lint", "status": "completed", "conclusion": "success"}
        ]});
        let states = |jobs: Vec<Job>| jobs.into_iter().map(|j| j.state).collect::<Vec<_>>();
        assert_eq!(
            states(github_jobs(&github).unwrap()),
            [JobState::Failed, JobState::Running, JobState::Finished]
        );
        assert!(
            github_jobs(&json!({"total_count": 4, "check_runs": github["check_runs"]})).is_err()
        );

        let forgejo = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "error", "target_url": "/t/r/actions/runs/1/jobs/0"},
            {"context": "ci / docs", "status": "pending"}
        ]});
        let jobs = forgejo_jobs(&forgejo).unwrap();
        assert_eq!(jobs[0].target, "/t/r/actions/runs/1/jobs/0");
        assert_eq!(states(jobs), [JobState::Failed, JobState::Running]);
        assert!(
            forgejo_jobs(&json!({"total_count": 0, "statuses": null}))
                .unwrap()
                .is_empty()
        );

        // A retried job's newest attempt decides; its failed first one is history.
        let gitlab = [
            json!({"id": 7, "name": "rust", "status": "failed", "target_url": "old"}),
            json!({"id": 9, "name": "rust", "status": "running", "target_url": "new"}),
            json!({"id": 8, "name": "docs", "status": "canceled"}),
        ];
        let jobs = gitlab_jobs(&gitlab).unwrap();
        assert_eq!(
            jobs.iter()
                .map(|j| (j.name.as_str(), j.state))
                .collect::<Vec<_>>(),
            [("docs", JobState::Failed), ("rust", JobState::Running)]
        );
        assert_eq!(jobs[1].target, "new");
        assert!(gitlab_jobs(&[json!({"id": 1, "name": "x", "status": "later"})]).is_err());
    }
}
