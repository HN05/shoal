//! Shared outcomes of daemon-owned resource allocation.
pub use shoal_daemon_base::allocation::Allocation;

use crate::{Leases, daemon::Daemon};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::space_names::SpaceName;

impl Leases {
    /// Record allocation notifications after the claim transaction has finished.
    pub async fn notify_allocation<T>(
        &self,
        daemon: &dyn Daemon,
        space: (&SpaceName, &SpaceId),
        outcome: &Allocation<T>,
        busy_prefix: &str,
    ) {
        match outcome {
            Allocation::Granted(_) => {}
            Allocation::Busy(message) => {
                let message = format!("{busy_prefix}{message}");
                (self.notify(daemon, space, AlertKind::ResourceBusy, message)).await;
            }
            Allocation::Approval(request) => daemon.announce(request).await,
        }
    }
}
