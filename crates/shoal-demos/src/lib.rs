//! Demos and the viewers of simulator demos, as `docs/design/demos.md`
//! describes them.
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
