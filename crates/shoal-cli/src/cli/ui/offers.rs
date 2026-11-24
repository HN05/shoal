//! How the request a picker's command sends judges each space the picker
//! could offer, as the daemon of the machine that runs the space decides,
//! and the other joined machines' spaces the command reaches.
use std::collections::HashMap;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{hub::directory::HubView, model::Offer};

use super::Acting;
use crate::cli::{context::Context, everywhere};

/// The daemons' verdicts on the spaces a picker could offer, and the other
/// machines' spaces as the hub lists them, for a command that reaches them.
#[derive(Default)]
pub(super) struct Offers {
    judged: HashMap<SpaceId, Offer>,
    pub(super) elsewhere: Option<HubView>,
}

/// How a picker shows a space it offers.
pub(super) enum Shown<'a> {
    /// As the command would act on it.
    Taken,
    /// With the reason the command would refuse it.
    Refused(&'a str),
}

impl Offers {
    /// What the daemons say of the request `acting` sends: this machine's
    /// alone for a command that acts here.
    pub(super) async fn of(ctx: &Context, acting: &Acting) -> Self {
        if acting.acts_here() {
            return Self::here(ctx, acting).await;
        }
        let everywhere = everywhere::offered(ctx, acting.operation()).await;
        Self {
            judged: everywhere.offers,
            elsewhere: everywhere.elsewhere,
        }
    }

    /// What this machine's daemon says of the request `acting` sends, for a
    /// command that acts on this machine's spaces itself; every space is
    /// offered, with a warning, while it cannot say.
    async fn here(ctx: &Context, acting: &Acting) -> Self {
        let judged = crate::client::spaces::judged_here(&ctx.paths, acting.operation()).await;
        let judged = judged.unwrap_or_else(|error| {
            eprintln!("warning: the daemon cannot say which spaces this command takes: {error:#}");
            Vec::new()
        });
        Self {
            judged: (judged.into_iter())
                .map(|judged| (judged.space, judged.offer))
                .collect(),
            elsewhere: None,
        }
    }

    /// The other machines' spaces in `elsewhere`, which no daemon judged,
    /// for a picker whose spaces the command selects without asking.
    pub(super) fn unjudged(elsewhere: Option<HubView>) -> Self {
        Self {
            judged: HashMap::new(),
            elsewhere,
        }
    }

    /// The verdicts `judged` on the spaces of this machine and of the
    /// other machines in `elsewhere`.
    #[cfg(test)]
    pub(super) fn new(judged: HashMap<SpaceId, Offer>, elsewhere: Option<HubView>) -> Self {
        Self { judged, elsewhere }
    }

    /// How a picker shows `space`; `None` leaves it out. A space no daemon
    /// judged, as while its machine cannot be asked, is offered.
    pub(super) fn shown(&self, space: &SpaceId) -> Option<Shown<'_>> {
        match self.judged.get(space) {
            None | Some(Offer::Accepted) => Some(Shown::Taken),
            Some(Offer::Refused { reason }) => Some(Shown::Refused(reason)),
            Some(Offer::Lacking) => None,
        }
    }
}
