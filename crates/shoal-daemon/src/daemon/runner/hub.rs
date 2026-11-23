//! The hub the runner hands the CI jobs this machine cut off.

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_protocol::hub::reruns::CutOffJob;
use shoal_runner::daemon::Hub;

use crate::daemon::manager::Manager;

impl Hub for Manager {
    fn hand_cut_off_jobs(&self, jobs: Vec<CutOffJob>) -> BoxFuture<'_, Result<bool>> {
        Box::pin(self.hub.hand_cut_off_jobs(self, jobs))
    }
}
