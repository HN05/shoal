//! Platforms without a login manager that announces shutdowns, such as
//! macOS, where the signals of the shutdown itself stop the work.
use anyhow::Result;

use super::LoginManager;

pub(super) enum Unsupported {}

pub(super) async fn connect() -> Option<Unsupported> {
    None
}

impl LoginManager for Unsupported {
    type Lock = ();

    async fn delay_shutdown(&self) -> Result<()> {
        match *self {}
    }

    async fn announcement(&mut self) -> Option<bool> {
        match *self {}
    }
}
