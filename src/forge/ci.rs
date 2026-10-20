//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the workspace's agent account.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
    checks::validate_check_name,
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

/// A check on a PR's head under the name the forge shows, and the CI job it
/// reports when Shoal can act on that job.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    name: String,
    state: JobState,
    location: Option<Location>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Failed,
    Running,
    Finished,
}

/// A CI job in the repository, by the IDs its forge acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    GitHub {
        run: u64,
        job: u64,
    },
    GitLab {
        job: u64,
    },
    /// Forgejo links a run by its number in the repository, not its API ID.
    Forgejo {
        run_number: u64,
    },
}

/// A request that carries out an action.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    /// POST to this endpoint under the repository's.
    Post(String),
    CancelForgejoRun {
        run_number: u64,
    },
}

/// The repository endpoints to POST to, in order, and the checks they reach.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    endpoints: Vec<String>,
    checks: Vec<String>,
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
    /// workspace's repository, or every such CI job when none is named.
    pub async fn ci_action(
        &self,
        selector: &str,
        item: Option<String>,
        action: CiAction,
        checks: Vec<String>,
    ) -> Result<CiActed> {
        for check in &checks {
            validate_check_name(check)?;
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
        let plan = forge
            .plan_ci(&account, number, action, &checks)
            .await
            .with_context(|| format!("could not {action} checks of {url}"))?;
        let (sent, result) = forge.post_ci(&account, &plan.endpoints).await;
        if sent > 0 {
            self.record_ci_action(&workspace.id, &url, action).await?;
        }
        result.with_context(|| {
            format!(
                "could not {action} every check of {url}; {sent} of {} requests succeeded",
                plan.endpoints.len()
            )
        })?;
        Ok(CiActed {
            url,
            action,
            checks: plan.checks,
        })
    }

    async fn record_ci_action(&self, id: &str, url: &str, action: CiAction) -> Result<()> {
        let (id, url) = (id.to_owned(), url.to_owned());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Pr, &url, action.as_str()))
            .await
    }
}

impl ForgeRepo {
    /// What applying `action` to the named checks on the open PR's head, or to
    /// every CI job it applies to, sends and reaches.
    async fn plan_ci(
        &self,
        account: &Account,
        number: u64,
        action: CiAction,
        names: &[String],
    ) -> Result<Plan> {
        self.supports_ci_action(action)?;
        let head = self.open_head(account, number).await?;
        let jobs = self.ci_jobs(account, &head).await?;
        let selected = select(&jobs, action, names)?;
        let (operations, checks) = operations(&jobs, &selected, action)?;
        let mut endpoints = Vec::new();
        for operation in operations {
            push_new(
                &mut endpoints,
                self.endpoint(account, &head, operation).await?,
            );
        }
        Ok(Plan { endpoints, checks })
    }

    /// POST to each endpoint in order, stopping at the first failure, and
    /// return how many succeeded with the outcome.
    async fn post_ci(&self, account: &Account, endpoints: &[String]) -> (usize, Result<()>) {
        for (sent, endpoint) in endpoints.iter().enumerate() {
            let request = Request::new(HttpMethod::Post, self.repo_endpoint(endpoint), None);
            if let Err(error) = self.send(account, &request).await {
                return (sent, Err(error));
            }
        }
        (endpoints.len(), Ok(()))
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

    async fn open_head(&self, account: &Account, number: u64) -> Result<String> {
        let pull = self.read(account, ItemKind::Pr, number).await?;
        ensure!(pull.item.state == ItemState::Open, "#{number} is not open");
        pull.head_sha.context("PR has no head commit")
    }

    /// Each check's latest attempt on `head`.
    async fn ci_jobs(&self, account: &Account, head: &str) -> Result<Vec<Job>> {
        let checks = match self.kind {
            ForgeKind::GitHub => {
                // Only each check's latest attempt, not those reruns replaced.
                let endpoint = self.repo_endpoint(&format!(
                    "commits/{head}/check-runs?filter=latest&per_page=100"
                ));
                github_checks(&self.send(account, &Request::get(endpoint)).await?)?
            }
            ForgeKind::Forgejo => {
                let endpoint = self.repo_endpoint(&format!("commits/{head}/status?limit=50"));
                forgejo_checks(&self.send(account, &Request::get(endpoint)).await?)?
            }
            ForgeKind::GitLab => {
                let endpoint = self.repo_endpoint(&format!("repository/commits/{head}/statuses"));
                gitlab_checks(&self.pages(account, &endpoint).await?)?
            }
            ForgeKind::Bitbucket => bail!("Bitbucket Pipelines are not supported"),
        };
        Ok(checks
            .into_iter()
            .map(|(name, state, target)| Job {
                name,
                state,
                location: self.locate(&target),
            })
            .collect())
    }

    /// The CI job a check's web URL links in this repository, if any.
    fn locate(&self, target: &str) -> Option<Location> {
        match self.kind {
            ForgeKind::GitHub => {
                let parts = self.repository_path(target, "/actions/runs/")?;
                let [run, "job", job] = parts.as_slice() else {
                    return None;
                };
                Some(Location::GitHub {
                    run: id(run)?,
                    job: id(job)?,
                })
            }
            ForgeKind::GitLab => {
                let parts = self.repository_path(target, "/-/jobs/")?;
                let [job] = parts.as_slice() else {
                    return None;
                };
                Some(Location::GitLab { job: id(job)? })
            }
            ForgeKind::Forgejo => {
                let (run_number, _) = self.job_location(target).ok()?;
                Some(Location::Forgejo { run_number })
            }
            ForgeKind::Bitbucket => None,
        }
    }

    /// The path components of `target` after this repository's `section`.
    /// Forges link jobs over HTTPS even when the remote uses HTTP.
    fn repository_path<'a>(&self, target: &'a str, section: &str) -> Option<Vec<&'a str>> {
        let path = target
            .strip_prefix("https://")
            .or_else(|| target.strip_prefix("http://"))?
            .strip_prefix(&format!("{}/{}", self.host, self.path))?
            .strip_prefix(section)?;
        Some(path.split('/').collect())
    }

    async fn endpoint(
        &self,
        account: &Account,
        head: &str,
        operation: Operation,
    ) -> Result<String> {
        Ok(match operation {
            Operation::Post(endpoint) => endpoint,
            Operation::CancelForgejoRun { run_number } => {
                let run = self.forgejo_run_id(account, run_number, head).await?;
                format!("actions/runs/{run}/cancel")
            }
        })
    }
}

fn id(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|id| *id > 0)
}

/// The jobs `action` applies to: those of the named checks, at least one per
/// name, or every CI job it applies to. Other providers' checks have no job
/// Shoal can act on.
fn select(jobs: &[Job], action: CiAction, names: &[String]) -> Result<Vec<Job>> {
    let (state, described) = action.applies_to();
    if names.is_empty() {
        let selected = jobs
            .iter()
            .filter(|job| job.state == state && job.location.is_some())
            .cloned()
            .collect::<Vec<_>>();
        ensure!(
            !selected.is_empty(),
            "no CI job on the PR's head is {described}"
        );
        return Ok(selected);
    }
    let mut selected = Vec::new();
    for (index, name) in names.iter().enumerate() {
        // A repeated name selects its jobs once.
        if names[..index].contains(name) {
            continue;
        }
        let named = jobs.iter().filter(|job| job.name == *name);
        ensure!(
            named.clone().next().is_some(),
            "check {name} was not found on the PR's current head"
        );
        let matching = named.filter(|job| job.state == state).collect::<Vec<_>>();
        ensure!(!matching.is_empty(), "check {name} is not {described}");
        ensure!(
            matching.iter().all(|job| job.location.is_some()),
            "check {name} does not link to a CI job in this repository"
        );
        selected.extend(matching.into_iter().cloned());
    }
    Ok(selected)
}

/// The operations that apply `action` to `selected` jobs among all of the
/// head's `jobs`, and the checks they reach. A GitHub job rerun starts a new
/// attempt of its run, which then refuses its other jobs, so several failed
/// jobs of one run rerun together with the rest of its failed jobs. GitHub
/// reruns only finished runs.
fn operations(
    jobs: &[Job],
    selected: &[Job],
    action: CiAction,
) -> Result<(Vec<Operation>, Vec<String>)> {
    let mut operations = Vec::new();
    let mut checks = Vec::new();
    for job in selected {
        let (operation, reached) = operation(jobs, selected, job, action)?;
        push_new(&mut operations, operation);
        for name in reached {
            push_new(&mut checks, name);
        }
    }
    Ok((operations, checks))
}

/// The operation that applies `action` to `job`, and the checks it reaches.
fn operation(
    jobs: &[Job],
    selected: &[Job],
    job: &Job,
    action: CiAction,
) -> Result<(Operation, Vec<String>)> {
    let location = job.location.context("check has no CI job")?;
    let in_run = |run| move |other: &&Job| github_run(other) == Some(run);
    let only = |operation| Ok((operation, vec![job.name.clone()]));
    if let (Location::GitHub { run, .. }, CiAction::Rerun) = (location, action) {
        // GitHub reruns only finished runs.
        let running = jobs
            .iter()
            .filter(in_run(run))
            .find(|other| other.state == JobState::Running);
        if let Some(running) = running {
            bail!(
                "check {} is still running in the same workflow run; wait for it or cancel it before rerunning {}",
                running.name,
                job.name
            );
        }
    }
    match (location, action) {
        (Location::GitHub { run, .. }, CiAction::Rerun)
            if selected.iter().filter(in_run(run)).count() > 1 =>
        {
            let failed = jobs
                .iter()
                .filter(in_run(run))
                .filter(|other| other.state == JobState::Failed);
            Ok((
                Operation::Post(format!("actions/runs/{run}/rerun-failed-jobs")),
                failed.map(|other| other.name.clone()).collect(),
            ))
        }
        (Location::GitHub { job: id, .. }, CiAction::Rerun) => {
            only(Operation::Post(format!("actions/jobs/{id}/rerun")))
        }
        (Location::GitHub { run, .. }, CiAction::Cancel) => {
            only(Operation::Post(format!("actions/runs/{run}/cancel")))
        }
        (Location::GitLab { job: id }, CiAction::Rerun) => {
            only(Operation::Post(format!("jobs/{id}/retry")))
        }
        (Location::GitLab { job: id }, CiAction::Cancel) => {
            only(Operation::Post(format!("jobs/{id}/cancel")))
        }
        (Location::Forgejo { run_number }, CiAction::Cancel) => {
            only(Operation::CancelForgejoRun { run_number })
        }
        (Location::Forgejo { .. }, CiAction::Rerun) => {
            bail!("Forgejo's API cannot rerun Actions jobs")
        }
    }
}

/// Jobs of one run share its cancellation or rerun.
fn push_new<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn github_run(job: &Job) -> Option<u64> {
    match job.location {
        Some(Location::GitHub { run, .. }) => Some(run),
        _ => None,
    }
}

/// A check's name, state and web URL.
type Check = (String, JobState, String);

/// GitHub's latest check runs.
fn github_checks(response: &Value) -> Result<Vec<Check>> {
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
            check(run, "name", state, "details_url")
        })
        .collect()
}

/// Forgejo's latest status of each check.
fn forgejo_checks(combined: &Value) -> Result<Vec<Check>> {
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
            check(status, "context", state, "target_url")
        })
        .collect()
}

/// GitLab's newest status of each name; a retried job keeps its earlier ones.
fn gitlab_checks(statuses: &[Value]) -> Result<Vec<Check>> {
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
                | "scheduled"
                | "waiting_for_callback",
            ) => JobState::Running,
            Some("failed" | "canceled") => JobState::Failed,
            Some("success" | "skipped" | "manual" | "canceling") => JobState::Finished,
            state => bail!("unknown GitLab CI state {state:?}"),
        };
        let check = check(status, "name", state, "target_url")?;
        if latest
            .get(&check.0)
            .is_none_or(|(newest, _): &(u64, Check)| *newest < id)
        {
            latest.insert(check.0.clone(), (id, check));
        }
    }
    Ok(latest.into_values().map(|(_, check)| check).collect())
}

fn check(value: &Value, name: &str, state: JobState, target: &str) -> Result<Check> {
    let name = value[name]
        .as_str()
        .filter(|name| !name.is_empty())
        .context("CI check has no name")?;
    let target = value[target].as_str().unwrap_or_default();
    Ok((name.into(), state, target.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(name: &str, state: JobState, location: Option<Location>) -> Job {
        Job {
            name: name.into(),
            state,
            location,
        }
    }

    fn github(run: u64, job: u64) -> Option<Location> {
        Some(Location::GitHub { run, job })
    }

    fn names(jobs: Vec<Job>) -> Vec<String> {
        jobs.into_iter().map(|job| job.name).collect()
    }

    #[test]
    fn actions_select_named_checks_in_their_state_or_every_ci_job() {
        let gitlab = Some(Location::GitLab { job: 1 });
        let jobs = [
            job("rust", JobState::Failed, gitlab),
            job("docs", JobState::Running, gitlab),
            job("lint", JobState::Finished, gitlab),
            job("rust", JobState::Finished, gitlab),
            job("external", JobState::Failed, None),
        ];
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
        let error = select(&jobs, CiAction::Rerun, &["external".into()]).unwrap_err();
        assert!(error.to_string().contains("does not link"), "{error}");
        let error = select(&jobs[2..], CiAction::Rerun, &[]).unwrap_err();
        assert_eq!(error.to_string(), "no CI job on the PR's head is failed");
    }

    #[test]
    fn github_reruns_several_failed_jobs_of_a_run_together() {
        let jobs = [
            job("rust", JobState::Failed, github(1, 11)),
            job("docs", JobState::Failed, github(1, 12)),
            job("lint", JobState::Failed, github(1, 13)),
            job("fmt", JobState::Failed, github(2, 21)),
            job("test", JobState::Running, github(3, 31)),
            job("build", JobState::Failed, github(3, 32)),
        ];
        let post = |endpoint: &str| Operation::Post(endpoint.into());
        let selected = [jobs[0].clone(), jobs[1].clone(), jobs[3].clone()];
        let (planned, checks) = operations(&jobs, &selected, CiAction::Rerun).unwrap();
        assert_eq!(
            planned,
            [
                post("actions/runs/1/rerun-failed-jobs"),
                post("actions/jobs/21/rerun")
            ]
        );
        assert_eq!(checks, ["rust", "docs", "lint", "fmt"]);
        let repeated = select(&jobs, CiAction::Rerun, &["rust".into(), "rust".into()]).unwrap();
        let (planned, checks) = operations(&jobs, &repeated, CiAction::Rerun).unwrap();
        assert_eq!(planned, [post("actions/jobs/11/rerun")]);
        assert_eq!(checks, ["rust"]);
        let (planned, _) = operations(&jobs, &jobs[4..5], CiAction::Cancel).unwrap();
        assert_eq!(planned, [post("actions/runs/3/cancel")]);
        // A run with a running job refuses reruns until it finishes.
        let error = operations(&jobs, &jobs[5..], CiAction::Rerun).unwrap_err();
        assert!(
            error.to_string().contains("test is still running"),
            "{error}"
        );
    }

    #[test]
    fn check_urls_locate_jobs_only_in_the_repository() {
        let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
        assert_eq!(
            github.locate("https://github.com/team/repo/actions/runs/12/job/34"),
            Some(Location::GitHub { run: 12, job: 34 })
        );
        for target in [
            "https://github.com/other/repo/actions/runs/12/job/34",
            "https://github.com/team/repo/actions/runs/12",
            "https://github.com/team/repo/actions/runs/0/job/34",
            "https://checks.example/team/repo/actions/runs/12/job/34",
            "",
        ] {
            assert_eq!(github.locate(target), None, "{target}");
        }
        let gitlab = ForgeRepo::parse("http://gitlab.com/group/sub/repo").unwrap();
        assert_eq!(
            gitlab.locate("https://gitlab.com/group/sub/repo/-/jobs/56"),
            Some(Location::GitLab { job: 56 })
        );
        for target in [
            "https://gitlab.com/group/repo/-/jobs/56",
            "https://gitlab.com/group/sub/repo/-/pipelines/56",
            "https://gitlab.com/group/sub/repo/-/jobs/56/retry",
        ] {
            assert_eq!(gitlab.locate(target), None, "{target}");
        }
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            forgejo.locate("/team/repo/actions/runs/2765/jobs/1"),
            Some(Location::Forgejo { run_number: 2765 })
        );
        assert_eq!(forgejo.locate("https://ci.example/build/1"), None);
    }

    #[test]
    fn each_forge_reports_failed_and_running_checks() {
        let states = |checks: Vec<Check>| checks.into_iter().map(|c| c.1).collect::<Vec<_>>();
        let github = json!({"total_count": 3, "check_runs": [
            {"name": "rust", "status": "completed", "conclusion": "timed_out", "details_url": "u"},
            {"name": "docs", "status": "queued", "conclusion": null},
            {"name": "lint", "status": "completed", "conclusion": "success"}
        ]});
        assert_eq!(
            states(github_checks(&github).unwrap()),
            [JobState::Failed, JobState::Running, JobState::Finished]
        );
        let partial = json!({"total_count": 4, "check_runs": github["check_runs"]});
        assert!(github_checks(&partial).is_err());

        let forgejo = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "error", "target_url": "/t/r/actions/runs/1/jobs/0"},
            {"context": "ci / docs", "status": "pending"}
        ]});
        let checks = forgejo_checks(&forgejo).unwrap();
        assert_eq!(checks[0].2, "/t/r/actions/runs/1/jobs/0");
        assert_eq!(states(checks), [JobState::Failed, JobState::Running]);
        let none = json!({"total_count": 0, "statuses": null});
        assert!(forgejo_checks(&none).unwrap().is_empty());

        // A retried job's newest attempt decides; its failed first one is history.
        let gitlab = [
            json!({"id": 7, "name": "rust", "status": "failed", "target_url": "old"}),
            json!({"id": 9, "name": "rust", "status": "running", "target_url": "new"}),
            json!({"id": 8, "name": "docs", "status": "canceled"}),
            json!({"id": 10, "name": "deploy", "status": "waiting_for_callback"}),
        ];
        assert_eq!(
            gitlab_checks(&gitlab).unwrap(),
            [
                ("deploy".into(), JobState::Running, String::new()),
                ("docs".into(), JobState::Failed, String::new()),
                ("rust".into(), JobState::Running, "new".into())
            ]
        );
        let unknown = [json!({"id": 1, "name": "x", "status": "later"})];
        assert!(gitlab_checks(&unknown).is_err());
    }
}
