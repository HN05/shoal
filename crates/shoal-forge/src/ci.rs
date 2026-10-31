//! Rerunning failed and cancelling running CI jobs on a PR's current head,
//! as the workspace's agent account.
pub mod approval;
pub mod checks;
pub mod run;
pub mod run_action;
pub mod run_view;
pub mod workflow;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::states;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{item::ItemState, link::ItemKind},
    transport::api::{HttpMethod, Request},
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
pub(crate) struct Job {
    pub(crate) name: String,
    pub(crate) state: JobState,
    pub(crate) location: Option<Location>,
}

states!(
    /// Whether a CI job or run is still running and, once it finished,
    /// whether it failed.
    JobState {
        Failed => "failed",
        Running => "running",
        Finished => "finished",
    }
);

/// A CI job in the repository, by the IDs its forge acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Location {
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
pub(crate) enum Operation {
    /// POST to this endpoint under the repository's.
    Post(String),
    CancelForgejoRun {
        run_number: u64,
    },
}

/// The repository endpoints to POST to, in order, and the checks they reach.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub endpoints: Vec<String>,
    pub checks: Vec<String>,
}

impl CiAction {
    /// The state of the jobs the action applies to.
    pub(crate) fn applies_to(self) -> (JobState, &'static str) {
        match self {
            Self::Rerun => (JobState::Failed, "failed"),
            Self::Cancel => (JobState::Running, "running"),
        }
    }
}

impl ForgeRepo {
    /// What applying `action` to the named checks on the open PR's head, or to
    /// every CI job it applies to, sends and reaches.
    pub async fn plan_ci(
        &self,
        account: &Account,
        number: u64,
        action: CiAction,
        names: &[String],
    ) -> Result<Plan> {
        self.require(Capability::ci(action))?;
        let head = self.open_head(account, number).await?;
        let jobs = self.ci_jobs(account, &head).await?;
        let selected = select(&jobs, action, names)?;
        let (operations, checks) = operations(&self.kind, &jobs, &selected, action)?;
        let mut endpoints = Vec::new();
        for operation in operations {
            let endpoint = self
                .kind
                .ci_endpoint(self, account, &head, operation)
                .await?;
            push_new(&mut endpoints, endpoint);
        }
        Ok(Plan { endpoints, checks })
    }

    /// POST to each endpoint in order, stopping at the first failure, and
    /// return how many succeeded with the outcome.
    pub async fn post_ci(&self, account: &Account, endpoints: &[String]) -> (usize, Result<()>) {
        for (sent, endpoint) in endpoints.iter().enumerate() {
            let request = Request::new(HttpMethod::Post, self.repo_endpoint(endpoint), None);
            if let Err(error) = self.send(account, &request).await {
                return (sent, Err(error));
            }
        }
        (endpoints.len(), Ok(()))
    }

    /// Each check's latest attempt on `head`, with the job it links.
    async fn ci_jobs(&self, account: &Account, head: &str) -> Result<Vec<Job>> {
        let checks = self.kind.ci_checks(self, account, head).await?;
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
        self.kind.locate_job(self, target)
    }

    pub(crate) async fn open_head(&self, account: &Account, number: u64) -> Result<String> {
        let pull = self.read(account, ItemKind::Pr, number).await?;
        ensure!(pull.item.state == ItemState::Open, "#{number} is not open");
        pull.head_sha.context("PR has no head commit")
    }

    /// The path components of `target` after this repository's `section`.
    /// Forges link jobs over HTTPS even when the remote uses HTTP.
    pub(crate) fn repository_path<'a>(
        &self,
        target: &'a str,
        section: &str,
    ) -> Option<Vec<&'a str>> {
        let path = target
            .strip_prefix("https://")
            .or_else(|| target.strip_prefix("http://"))?
            .strip_prefix(&format!("{}/{}", self.host, self.path))?
            .strip_prefix(section)?;
        Some(path.split('/').collect())
    }
}

/// A positive ID in a job URL.
pub(crate) fn id(text: &str) -> Option<u64> {
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
/// head's `jobs`, as `backend` plans each, and the checks they reach; jobs
/// that share an operation share it once.
fn operations(
    backend: &impl Backend,
    jobs: &[Job],
    selected: &[Job],
    action: CiAction,
) -> Result<(Vec<Operation>, Vec<String>)> {
    let mut operations = Vec::new();
    let mut checks = Vec::new();
    for job in selected {
        let (operation, reached) = backend.ci_operation(jobs, selected, job, action)?;
        push_new(&mut operations, operation);
        for name in reached {
            push_new(&mut checks, name);
        }
    }
    Ok((operations, checks))
}

/// Jobs of one run share its cancellation or rerun.
pub(crate) fn push_new<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

/// A check's name, state and web URL.
pub(crate) type CiCheck = (String, JobState, String);

/// A check from the fields of a forge's check or status that hold its name
/// and web URL.
pub(crate) fn check(value: &Value, name: &str, state: JobState, target: &str) -> Result<CiCheck> {
    let name = value[name]
        .as_str()
        .filter(|name| !name.is_empty())
        .context("CI check has no name")?;
    let target = value[target].as_str().unwrap_or_default();
    Ok((name.into(), state, target.into()))
}

#[cfg(test)]
mod tests;
