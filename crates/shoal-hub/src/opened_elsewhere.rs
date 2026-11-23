//! The threads a space opened that other machines keep, its comments of
//! the reviews they keep of their workspaces: how many wait, which keeps
//! the space from cleanup as its waiting threads here do.
use shoal_ids::space_ids::SpaceId;

use crate::{Hub, daemon::Daemon, log};

impl Hub {
    /// How many threads the space `id` opened on other machines wait for
    /// their holder, as the hub lists them, or as it last did while it
    /// cannot be asked; none while neither can be read.
    pub async fn waiting_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> usize {
        let (machine, directory) = match self.directory_or_known(daemon).await {
            Ok(Some(known)) => known,
            Ok(None) => return 0,
            Err(error) => {
                log!("hub: could not read the threads {id} opened elsewhere: {error:#}");
                return 0;
            }
        };
        (directory.spaces.iter())
            .filter(|listed| listed.machine != machine)
            .flat_map(|listed| &listed.space.threads)
            .filter(|thread| thread.opener_id() == Some(id) && thread.is_waiting())
            .count()
    }
}
