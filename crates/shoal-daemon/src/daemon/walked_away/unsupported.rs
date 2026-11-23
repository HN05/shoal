//! Platforms whose lid and idle state Shoal does not read, where the user
//! always counts as present.
use anyhow::Result;

use super::Platform;

pub(super) enum Unsupported {}

pub(super) async fn connect() -> Option<Unsupported> {
    None
}

impl Platform for Unsupported {
    async fn walked_away(&mut self) -> Result<bool> {
        match *self {}
    }
}
