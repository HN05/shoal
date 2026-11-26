//! Platforms that announce no sleep.
use super::{PowerEvent, PowerEvents};

pub enum Unsupported {}

pub async fn connect() -> Option<Unsupported> {
    None
}

impl PowerEvents for Unsupported {
    type Hold = ();

    async fn next(&mut self) -> Option<PowerEvent<()>> {
        match *self {}
    }
}
