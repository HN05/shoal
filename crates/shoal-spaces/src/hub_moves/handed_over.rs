//! Continuing here a space another machine handed over as it shut down, or
//! that the hub took over from a machine that slept, from the package the
//! hub keeps of it. See `docs/design/hub.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::packages::Departure;

use super::receiving::Package;
use crate::{
    alerts::AlertKind,
    transfer::{Claim, ImportedMove},
};

impl Spaces {
    /// Queues its agents' restores. An import whose claim the hub took is
    /// this machine's space even when it then fails to open, which the user
    /// hears of here. Any other failure leaves the space on `from`.
    pub async fn continue_handed_over(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        from: &str,
        departure: Departure,
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        daemon.fetch_package(id, &file).await?;
        let package = Package::File(std::fs::File::open(&file)?);
        match self.import_moved(package, &Claim::Takeover, true).await? {
            ImportedMove::Arrived { arrived, carried } => {
                let arrived = std::iter::once(&arrived.space.id).chain(&carried);
                self.tell_arrived(daemon, arrived, from).await;
                Ok(())
            }
            ImportedMove::Failed { error } => {
                let left = match departure {
                    Departure::HandedOver => format!("which machine {from} handed over"),
                    Departure::Slept => {
                        format!("which the hub took over from machine {from} as it slept")
                    }
                };
                let message =
                    format!("Space {id}, {left}, is this machine's now but did not open: {error}");
                daemon
                    .coordination()
                    .notify(daemon, None, AlertKind::HandoverFailed, message)
                    .await;
                Ok(())
            }
            ImportedMove::Refused { error } => bail!("{error}"),
            ImportedMove::Recorded { .. } => bail!("the import here recorded the space as removed"),
        }
    }
}
