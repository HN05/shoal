//! The request a command sends for the space a picker picks, by which the
//! daemon decides which of the other joined machines' spaces the picker
//! offers beside this machine's.
use shoal_protocol::{protocol::Operation, space_names::SpaceSelector};

use crate::client::connection::Sendable;

/// What a picker's command does with the space it picks: the request it
/// sends for it.
pub struct Acting(Box<dyn Fn() -> Operation + Send + Sync>);

impl Acting {
    /// The command sends what `request` builds for the picked space.
    pub fn request<R: Sendable>(
        request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static,
    ) -> Self {
        let picking = move || request(crate::client::spaces::picking()).into_operation();
        Self(Box::new(picking))
    }

    /// The request, for a space the picker has yet to pick.
    pub(super) fn operation(&self) -> Operation {
        (self.0)()
    }
}
