//! The machine the hub chose for a space another machine handed over as it
//! shut down: it fetches the package the hub keeps of the space and
//! continues it through `shoal internal import-moved`, as a move through
//! the hub arrives, its claim taking the space over, then tells the
//! space's agent where it is now.
use anyhow::{Result, bail};
use shoal_ids::space_ids::SpaceId;

use super::receiving::Package;
use crate::{
    daemon::{alerts::AlertKind, manager::Manager},
    transfer::{Claim, ImportedMove},
};

impl Manager {
    /// Continue here the space `id`, which the machine named `from` handed
    /// over, from the package the hub keeps of it, queueing its agents'
    /// restores. An import the hub took the claim of is this machine's
    /// space even when it then fails to open, which the user hears of
    /// here; any other failure leaves the space on `from`.
    pub(crate) async fn continue_handed_over(&self, id: &SpaceId, from: &str) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        self.hub.fetch_package(self, id, &file).await?;
        let package = Package::File(std::fs::File::open(&file)?);
        match self.import_moved(package, &Claim::Takeover, true).await? {
            ImportedMove::Arrived { arrived, carried } => {
                let arrived = std::iter::once(&arrived.space.id).chain(&carried);
                self.tell_arrived(arrived, from).await;
                Ok(())
            }
            ImportedMove::Failed { error } => {
                let message = format!(
                    "Space {id}, which machine {from} handed over as it shut down, is this machine's now but did not open: {error}"
                );
                self.notify(None, AlertKind::HandoverFailed, message).await;
                Ok(())
            }
            ImportedMove::Refused { error } => bail!("{error}"),
            ImportedMove::Recorded { .. } => bail!("the import here recorded the space as removed"),
        }
    }
}
