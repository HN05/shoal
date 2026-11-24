//! Platforms whose lid and idle state Shoal does not read, where the user
//! always counts as present.
use anyhow::Result;

use super::{Lid, Platform};

pub enum Unsupported {}

pub async fn connect() -> Option<Unsupported> {
    None
}

impl Platform for Unsupported {
    async fn lid(&mut self) -> Result<Option<Lid>> {
        match *self {}
    }

    async fn idle(&mut self) -> Result<bool> {
        match *self {}
    }
}
