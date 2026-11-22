//! The request a command sends for the space a picker picks, by which the
//! daemon decides which of the other joined machines' spaces the picker
//! offers beside this machine's.
use shoal_protocol::{protocol::Operation, space_names::SpaceSelector};

use crate::client::connection::Sendable;

/// What a picker's command does with the space it picks.
pub enum Acting {
    /// Sends the request this builds for it.
    Request(Box<dyn Fn() -> Operation + Send + Sync>),
    /// Says nothing yet, so the picker offers the other machines' spaces as
    /// the command's name decides.
    Unconverted,
}

impl Acting {
    /// The command sends what `request` builds for the picked space.
    pub fn request<R: Sendable>(
        request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static,
    ) -> Self {
        let picking = move || request(crate::client::spaces::picking()).into_operation();
        Self::Request(Box::new(picking))
    }
}
