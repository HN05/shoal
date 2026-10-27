//! Failed CI job logs resolved from a PR's current check, using its agent account.
pub(crate) mod plain;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    ci::{CiCheck, JobState, Location},
    items::link::ItemKind,
    transport::api::Request,
};

/// The most of a log's end a [`CheckLog`] keeps, in bytes.
pub const TAIL_BYTES: usize = 8192;
/// The longest job URL a [`CheckLog`] keeps, in bytes.
pub const JOB_URL_BYTES: usize = 512;
/// The most of a job log Shoal downloads, in bytes.
pub(crate) const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLog {
    /// The PR's.
    pub url: String,
    pub name: String,
    /// The web page of the check's job, unless longer than [`JOB_URL_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_url: Option<String>,
    pub text: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
}

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

/// Refuse a check name no forge shows, before any lookup.
pub fn validate_check_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 1024,
        "check name must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

impl ForgeRepo {
    pub async fn failed_check_log(
        &self,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        self.kind.failed_check_log(self, account, url, name).await
    }

    /// The log of the CI job the failed check `name` on the current head of
    /// PR `url` links, read from the endpoint the forge serves it at.
    pub(crate) async fn failed_job_log(
        &self,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        let head = self.pull_head(account, url).await?;
        let checks = self.kind.ci_checks(self, account, &head).await?;
        let (location, target) = self.failed_job(&checks, name)?;
        let log = self.job_log(account, location).await?;
        let step = self.kind.failed_step(&log);
        Ok(CheckLog::from_text(url, name, &log, step).with_job(target))
    }

    /// The head commit of PR `url`, open or not.
    async fn pull_head(&self, account: &Account, url: &str) -> Result<String> {
        let (number, _) = self.pull(url)?;
        let pull = self.read(account, ItemKind::Pr, number).await?;
        pull.head_sha.context("PR has no head commit")
    }

    /// The log of the job at `location` as plain text, refused beyond
    /// [`MAX_LOG_BYTES`].
    pub(crate) async fn job_log(&self, account: &Account, location: Location) -> Result<String> {
        let endpoint = self.repo_endpoint(&self.kind.job_log_endpoint(location)?);
        let request = Request::get(endpoint).limited(MAX_LOG_BYTES);
        let log = self
            .read_text(account, &request)
            .await
            .context("fetch job log (maximum 8 MiB)")?;
        Ok(self.kind.plain_log(&log))
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

impl CheckLog {
    pub(crate) fn from_text(
        url: &str,
        name: &str,
        text: &str,
        failed_step: Option<String>,
    ) -> Self {
        let mut start = text.len().saturating_sub(TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Self {
            url: url.into(),
            name: name.into(),
            job_url: None,
            text: text[start..].into(),
            truncated: start > 0,
            failed_step,
        }
    }

    pub(crate) fn with_job(self, job_url: String) -> Self {
        Self {
            job_url: Some(job_url).filter(|url| url.len() <= JOB_URL_BYTES),
            ..self
        }
    }
}

#[cfg(test)]
mod tests;
