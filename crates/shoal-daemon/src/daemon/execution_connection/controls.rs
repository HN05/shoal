//! The send half of a wrapper's connection, which the relay and the space's
//! status reports share: each control goes out whole, never between the
//! bytes of another.
use anyhow::Result;
use tokio::{net::unix::OwnedWriteHalf, sync::Mutex};

use crate::protocol::{self, Control};

pub(in crate::daemon) struct Controls(Mutex<OwnedWriteHalf>);

impl Controls {
    pub(super) fn new(writer: OwnedWriteHalf) -> Self {
        Self(Mutex::new(writer))
    }

    /// Send `control` once no other control is being sent.
    pub(in crate::daemon) async fn send(&self, control: &Control) -> Result<()> {
        protocol::write(&mut *self.0.lock().await, control).await
    }
}
