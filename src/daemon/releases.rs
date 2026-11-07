//! Removing the release copies execution wrappers no longer run from.
use std::time::SystemTime;

use anyhow::Result;

use crate::{
    daemon::{log, workspace::Manager},
    fsutil::off_runtime,
    releases::Releases,
};

impl Manager {
    /// Remove the release copies no execution wrapper holds.
    pub(crate) async fn sweep_releases(&self) -> Result<()> {
        let releases = Releases::new(self.paths.releases());
        let removed = off_runtime::run(move || releases.sweep(SystemTime::now())).await?;
        if !removed.is_empty() {
            log!("removed release copies {}", removed.join(", "));
        }
        Ok(())
    }
}
