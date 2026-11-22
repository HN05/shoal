//! The request a command sends for the space a picker picks, by which the
//! daemon judges each space the picker could offer, and decides which of
//! the other joined machines' spaces it offers beside this machine's.
use shoal_protocol::{protocol::Operation, space_names::SpaceSelector};

use crate::client::connection::Sendable;

/// What a picker's command does with the space it picks: the request it
/// sends for it, or `None` when the command acts on it itself, here.
pub struct Acting(Option<Box<dyn Fn() -> Operation + Send + Sync>>);

impl Acting {
    /// The command sends what `request` builds for the picked space.
    pub fn request<R: Sendable>(
        request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static,
    ) -> Self {
        let picking = move || request(crate::client::spaces::picking()).into_operation();
        Self(Some(Box::new(picking)))
    }

    /// The command acts on the picked space in its own process, which
    /// reaches only this machine's: it runs there, types into its terminal
    /// or reads its files.
    pub fn here() -> Self {
        Self(None)
    }

    /// The request, for a space the picker has yet to pick; `None` when
    /// the command acts here.
    pub(super) fn operation(&self) -> Option<Operation> {
        self.0.as_ref().map(|request| request())
    }
}
