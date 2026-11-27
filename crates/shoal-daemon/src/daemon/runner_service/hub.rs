//! The hub the runner hands the CI jobs this machine cut off, and asks
//! where this machine ranks for a waiting one.

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_protocol::{hub::reruns::CutOffJob, model::Repository};
use shoal_runner::daemon::Hub;

use crate::daemon::manager::Manager;

impl Hub for Manager {
    fn hand_cut_off_jobs(&self, jobs: Vec<CutOffJob>) -> BoxFuture<'_, Result<bool>> {
        Box::pin(self.hub.hand_cut_off_jobs(self, jobs))
    }

    fn rank_job<'a>(&'a self, repository: &'a Repository) -> BoxFuture<'a, Result<usize>> {
        Box::pin(self.hub.rank_job(self, repository))
    }
}
