//! What the spaces service needs of the rest of the daemon, which the
//! daemon implements here, one module per interface.
use shoal_coordination::Coordination;
use shoal_spaces::Spaces;

use crate::daemon::manager::Manager;

mod agents;
mod demos;
mod hub;
mod leases;
mod links;
mod operations;
mod records;
mod repositories;
mod reviews;
mod runner;

pub(crate) use records::SpaceRecords;

#[cfg(test)]
mod tests;
impl shoal_spaces::daemon::Daemon for Manager {
    fn spaces(&self) -> &Spaces {
        &self.spaces
    }

    fn coordination(&self) -> &Coordination {
        &self.coordination
    }
}
