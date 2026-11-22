//! How the request a picker's command sends judges each space the picker
//! could offer, as the daemon of the machine that runs the space decides,
//! and the other joined machines' spaces the command reaches.
use std::collections::HashMap;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{hub::directory::HubView, model::Offer};

use super::Acting;
use crate::{
    cli::{
        availability::{self, Fit},
        context::Context,
        everywhere,
    },
    model::{Chat, Workspace},
};

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
    /// What the daemons say of the request `acting` sends; for a command
    /// that acts here, what the command's need says of this machine's
    /// `workspaces` and `chats`.
    pub(super) async fn of(
        ctx: &Context,
        acting: &Acting,
        workspaces: &[Workspace],
        chats: &[Chat],
    ) -> Self {
        let Some(acting) = acting.operation() else {
            return Self::needed(ctx, workspaces, chats).await;
        };
        let everywhere = everywhere::offered(ctx, acting).await;
        Self {
            judged: everywhere.offers,
            elsewhere: everywhere.elsewhere,
        }
    }

    /// What the command's need says of `workspaces` and `chats`, for a
    /// command that acts on this machine's spaces itself.
    async fn needed(ctx: &Context, workspaces: &[Workspace], chats: &[Chat]) -> Self {
        let availability = availability::load(ctx.need, &ctx.paths, workspaces).await;
        let workspaces = workspaces.iter().map(|workspace| {
            let offer = match availability.fit(workspace) {
                Fit::Accepted => Offer::Accepted,
                Fit::Refused(reason) => Offer::Refused { reason },
                Fit::Hidden => Offer::Lacking,
            };
            (workspace.id.clone(), offer)
        });
        let chats = chats.iter().map(|chat| {
            let offer = match ctx.need.offers_chat(chat) {
                true => Offer::Accepted,
                false => Offer::Lacking,
            };
            (chat.id.clone(), offer)
        });
        Self {
            judged: workspaces.chain(chats).collect(),
            elsewhere: None,
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
