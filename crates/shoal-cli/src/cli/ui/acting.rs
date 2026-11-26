//! What a picker's command does with the space it picks. The daemon judges each
//! space the picker could offer by the request the command sends.
use shoal_protocol::{protocol::Operation, space_names::SpaceSelector};

use crate::client::connection::Sendable;

/// What a picker's command does with the space it picks: the request it
/// sends for it, and whether it acts on the space in its own process too,
/// which reaches only this machine's.
pub struct Acting {
    request: Box<dyn Fn() -> Operation + Send + Sync>,
    here: bool,
}

impl Acting {
    /// The command sends what `request` builds for the picked space,
    /// wherever it runs.
    pub fn request<R: Sendable>(
        request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static,
    ) -> Self {
        Self::of(request, false)
    }

    /// The command sends what `request` builds for the picked space, and
    /// acts on it in its own process, which reaches only this machine's:
    /// it runs there, types into its terminal or reads its files.
    pub fn here<R: Sendable>(request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static) -> Self {
        Self::of(request, true)
    }

    fn of<R: Sendable>(
        request: impl Fn(SpaceSelector) -> R + Send + Sync + 'static,
        here: bool,
    ) -> Self {
        let picking = move || request(crate::client::spaces::picking()).into_operation();
        Self {
            request: Box::new(picking),
            here,
        }
    }

    /// The request, for a space the picker has yet to pick.
    pub(in crate::cli) fn operation(&self) -> Operation {
        (self.request)()
    }

    pub(super) fn acts_here(&self) -> bool {
        self.here
    }
}
