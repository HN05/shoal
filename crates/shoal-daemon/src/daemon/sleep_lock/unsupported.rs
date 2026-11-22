//! Platforms that announce no sleep, where the hub reads a sleeping
//! machine as one that dropped.
use super::{PowerEvent, PowerEvents};

pub(super) enum Unsupported {}

pub(super) async fn connect() -> Option<Unsupported> {
    None
}

impl PowerEvents for Unsupported {
    type Hold = ();

    async fn next(&mut self) -> Option<PowerEvent<()>> {
        match *self {}
    }
}
