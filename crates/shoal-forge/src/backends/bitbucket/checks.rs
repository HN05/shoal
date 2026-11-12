//! Bitbucket's CI checks on a commit, the actions on their jobs and their logs.
use anyhow::{Result, bail};

use super::Bitbucket;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Checks,
    ci::{CiAction, CiCheck, Job, Location, Operation},
};

impl Checks for Bitbucket {
    /// No CI job Shoal can act on, since it reads no Pipelines jobs.
    async fn ci_checks(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _head: &str,
    ) -> Result<Vec<CiCheck>> {
        Ok(Vec::new())
    }

    fn locate_job(&self, _repo: &ForgeRepo, _target: &str) -> Option<Location> {
        None
    }

    fn ci_operation(
        &self,
        _jobs: &[Job],
        _selected: &[Job],
        job: &Job,
        _action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        bail!("check {} is not a CI job Shoal can act on", job.name)
    }
}
