//! Demos the daemon records with the executions that run them, so a demo
//! ends with its execution: when it stops, its workspace is removed or its
//! wrapper finishes; and the viewers of simulator demos, which outlive the
//! demo's runs. [`Demos`] holds what they keep in memory and offers their
//! operations; what they need of the rest of the daemon is the interface in
//! [`daemon`]. The daemon relays a client's stream to a running demo.
use shoal_core::{env, paths, process, refusal};
use shoal_daemon_base::log;
use shoal_protocol::{model, time};
use shoal_store::Store;

pub mod daemon;
pub mod records;
mod running;
#[cfg(test)]
mod stand_in;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod viewers;

pub use viewers::connect;

/// The demos' service, which `Manager` holds.
pub struct Demos {
    store: Store,
    paths: paths::Paths,
    /// The locks of demos' viewers.
    viewers: viewers::Viewers,
}

impl Demos {
    pub fn new(store: Store, paths: paths::Paths) -> Self {
        Self {
            store,
            paths,
            viewers: viewers::Viewers::default(),
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
