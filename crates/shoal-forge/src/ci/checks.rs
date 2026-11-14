//! Failed CI job logs resolved from a PR's current check, using its agent account.
pub(crate) mod plain;

use anyhow::{Context, Result, ensure};
use shoal_forge_types::items::{link::LinkKind, reference::ItemRef};
use shoal_refs::CommitId;

use crate::{
    account::Account,
    backends::Checks,
    capability::CapabilityRequests,
    ci::{CiCheck, CiSteps, Location},
    items::action::ActionRequests,
    transport::api::{Request, TransportRequests},
};
use shoal_forge_types::{
    ForgeRepo,
    capability::Capability,
    ci::{JobState, checks::CheckLog},
    items::link::ItemKind,
    numbers::PullNumber,
    review::view::ItemView,
};

/// The most of a job log Shoal downloads, in bytes.
pub(crate) const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;

/// A check that reports no CI job in the repository, such as an external
/// service's, so it has no log Shoal can read.
#[derive(Debug)]
pub struct NoJobLog {
    pub name: String,
}

impl std::fmt::Display for NoJobLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "check {} is not a CI job in this repository, so Shoal cannot read its log; open its details on the forge",
            self.name
        )
    }
}

impl std::error::Error for NoJobLog {}

/// The characters of a log read's error an update about the failed check keeps.
const MAX_LOG_ERROR: usize = 512;

/// What an update about a failed check adds about its log `read`: the step
/// that failed, or why the log could not be read; `None` for a check with
/// no CI job, which has no log to point to.
pub fn failed_log_summary(read: Result<CheckLog>) -> Option<String> {
    Some(match read {
        Ok(log) => log
            .failed_step
            .map(|step| format!("; failed step: {step}"))
            .unwrap_or_default(),
        Err(error) if error.downcast_ref::<NoJobLog>().is_some() => return None,
        Err(error) => format!(
            "; log unavailable: {}",
            format!("{error:#}")
                .chars()
                .take(MAX_LOG_ERROR)
                .collect::<String>()
        ),
    })
}

/// Refuse a check name no forge shows, before any lookup.
pub fn validate_check_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 1024,
        "check name must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

/// Reading failed checks' logs and PR heads.
#[shoal_forge_derive::extension(pub CheckRequests, pub(crate) CheckSteps)]
impl ForgeRepo {
    pub async fn failed_check_log(
        &self,
        account: &Account,
        pull: &ItemRef,
        name: &str,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        pull.belongs_to(self)?;
        self.kind()
            .failed_check_log(self, account, pull, name)
            .await
    }

    /// Add the logs of the failed checks of the PRs among `views` in this
    /// repository, read as `account`, recording those it cannot read.
    pub async fn read_failed_logs(&self, account: &Account, views: &mut [ItemView]) {
        if !self.supports(Capability::ReadJobLogs) {
            return;
        }
        for view in views {
            // Another repository's PRs are read where their forge is.
            let pull = view.url.clone();
            if pull.kind() != LinkKind::Pr || pull.belongs_to(self).is_err() {
                continue;
            }
            let failed: Vec<String> = view
                .details
                .iter()
                .filter_map(|details| details.pr.as_ref())
                .flat_map(|pr| &pr.checks)
                .filter(|check| check.result.failed())
                .map(|check| check.name.clone())
                .collect();
            for name in failed {
                match self.failed_check_log(account, &pull, &name).await {
                    Ok(log) => view.check_logs.push(log),
                    Err(error) => view
                        .errors
                        .push(format!("{name}: log unavailable: {error:#}")),
                }
            }
        }
    }

    /// The log of the CI job the failed check `name` on the current head of
    /// PR `pull` links, read from the endpoint the forge serves it at.
    pub(crate) async fn failed_job_log(
        &self,
        account: &Account,
        pull: &ItemRef,
        name: &str,
    ) -> Result<CheckLog> {
        let head = self.pull_head(account, pull).await?;
        let checks = self.kind().ci_checks(self, account, head.as_str()).await?;
        let (location, target) = self.failed_job(&checks, name)?;
        let log = self.job_log(account, location).await?;
        let step = self.kind().failed_step(&log);
        Ok(CheckLog::from_text(pull.url(), name, &log, step).with_job(target))
    }

    /// The head commit of PR `pull`, open or not.
    pub async fn pull_head(&self, account: &Account, pull: &ItemRef) -> Result<CommitId> {
        pull.belongs_to(self)?;
        let pull = self.read(account, ItemKind::Pr, pull.pull()?.get()).await?;
        pull.head_sha.context("PR has no head commit")
    }

    /// The head commit of PR `number` of this repository, open or not.
    pub async fn numbered_pull_head(
        &self,
        account: &Account,
        number: PullNumber,
    ) -> Result<CommitId> {
        let pull = ItemRef::numbered(self, LinkKind::Pr, number.get())?;
        self.pull_head(account, &pull)
            .await
            .with_context(|| format!("read the head of {pull}"))
    }

    /// The log of the job at `location` as plain text, refused beyond
    /// [`MAX_LOG_BYTES`].
    pub(crate) async fn job_log(&self, account: &Account, location: Location) -> Result<String> {
        let endpoint = self.repo_endpoint(&self.kind().job_log_endpoint(location)?);
        // GitHub answers with a redirect to where it keeps the log.
        let request = Request::get(endpoint).limited(MAX_LOG_BYTES).redirected();
        let log = self
            .read_text(account, &request)
            .await
            .context("fetch job log (maximum 8 MiB)")?;
        Ok(self.kind().plain_log(&log))
    }

    /// The job and web URL of the failed check `name` among `checks`.
    fn failed_job(&self, checks: &[CiCheck], name: &str) -> Result<(Location, String)> {
        let mut named = checks.iter().filter(|check| check.0 == name).peekable();
        ensure!(
            named.peek().is_some(),
            "check {name} was not found on the PR's current head"
        );
        let (_, _, target) = named
            .find(|check| check.1 == JobState::Failed)
            .with_context(|| format!("check {name} has not failed"))?;
        let location = self
            .locate(target)
            .ok_or_else(|| NoJobLog { name: name.into() })?;
        Ok((location, target.clone()))
    }
}

#[cfg(test)]
mod tests;
