//! The hub's routing of what one machine sends another: a forwarded
//! request goes to the machine it names, and an event to the machine of
//! the space it is for, each answered by the hub's own machine for its own
//! spaces and passed down the link of any other; while that machine is
//! offline the hub holds what can wait and refuses the rest, naming the
//! machine and when it last saw it.
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    Member, MemberKind,
    link::{Call, Reply},
    routing::{Delivery, Expired, Forwarded, Told, Vouched},
};

use super::queue::Held;
use crate::{
    daemon::workspace::Manager,
    refusal::{Reason, Refusal},
    time::UnixTime,
};

impl Manager {
    /// The answer to `request`, which the member `from` forwards to the
    /// machine `machine`: a device's goes as the user's, whatever it
    /// vouches.
    pub(super) async fn route_request(
        &self,
        from: &Member,
        machine: &MemberId,
        mut request: Forwarded,
        delivery: Delivery,
    ) -> Result<Reply> {
        if from.kind == MemberKind::Device {
            request.caller = Vouched::User;
        }
        let sender_space = match &request.caller {
            Vouched::Space(space) => Some(space.space.clone()),
            Vouched::User => None,
        };
        let held = Held {
            sender: from.id.clone(),
            sender_space,
            summary: request.summary.clone(),
            call: Call::Serve { request },
        };
        let machine = self.hub_machine(machine).await?;
        self.send_or_hold(&machine, held, delivery == Delivery::Queued)
            .await
    }

    /// The answer to `told`, which the member `from` passes up the tree to
    /// the machine of the space it is for.
    pub(super) async fn route_told(&self, from: &MemberId, told: Told) -> Result<Reply> {
        let own = self.own_member().await?;
        let Some(machine) = self.local_space_owner(&told.above, &own).await? else {
            bail!("the hub lists no space {} to pass the event to", told.above);
        };
        let held = Held {
            sender: from.clone(),
            sender_space: None,
            summary: match &told.about {
                Some(about) => format!("an event of space {} for the space above it", about.name),
                None => format!("a message for space {}", told.above),
            },
            call: Call::Tell { told },
        };
        self.send_or_hold(&machine, held, true).await
    }

    /// Tell the machine `sender` what expired: answered at once for the
    /// hub's own machine, held while it is offline.
    pub(super) async fn send_expired(&self, sender: &MemberId, expired: Expired) -> Result<()> {
        let machine = self.hub_machine(sender).await?;
        let held = Held {
            sender: self.own_member().await?,
            sender_space: None,
            summary: format!("the expiry of {}", expired.summary),
            call: Call::Expired { expired },
        };
        self.send_or_hold(&machine, held, true).await.map(drop)
    }

    /// Send `held`'s call to `machine`, which answers it here when it is
    /// the hub's own machine, and hold it while that machine is offline
    /// when `waits`, or else refuse it.
    async fn send_or_hold(&self, machine: &Member, held: Held, waits: bool) -> Result<Reply> {
        if machine.hub {
            return self.answer_hub_call(held.call).await;
        }
        if let Some(calls) = self.hub.links.calls(&machine.id) {
            return calls.call(held.call).await;
        }
        if !waits {
            return Err(offline(machine).into());
        }
        Ok(Reply::Queued(self.hold_for(machine, held).await?))
    }

    /// The member `id`, a machine the hub records.
    async fn hub_machine(&self, id: &MemberId) -> Result<Member> {
        let members = self.local_hub_overview().await?.members;
        (members.into_iter())
            .find(|member| &member.id == id && member.kind == MemberKind::Machine)
            .ok_or_else(|| anyhow!("the hub has no machine {id}"))
    }

    /// This machine's answer to `call`, which the hub passes it, or which
    /// the hub's own machine answers itself: a forwarded request for one of
    /// its spaces, an event for one, or the expiry of what it sent.
    pub(super) async fn answer_hub_call(&self, call: Call) -> Result<Reply> {
        match call {
            Call::Serve { request } => Ok(Reply::Answered(self.serve_forwarded(request).await?)),
            Call::Tell { told } => {
                // Boxed: delivering it may pass it up through this hub again.
                Box::pin(self.deliver_told(told)).await?;
                Ok(Reply::Delivered)
            }
            Call::Expired { expired } => {
                self.hear_expired(expired).await;
                Ok(Reply::Delivered)
            }
            _ => bail!("a machine answers only what the hub passes it from other machines"),
        }
    }

    /// The answer a machine's daemon gives its link to the hub's `call`.
    pub(super) async fn answer_from_hub(self: Arc<Self>, call: Call) -> (Reply, ()) {
        let Some(_operation) = self.gates.operation_unless_exiting().await else {
            let message = "this machine's daemon is restarting; try again once it is back";
            return (
                Reply::Failed {
                    message: message.into(),
                },
                (),
            );
        };
        (super::session::reply(self.answer_hub_call(call).await), ())
    }
}

/// The refusal of a request for an offline machine's space, naming the
/// machine and when the hub last saw it.
pub(super) fn offline(machine: &Member) -> Refusal {
    let seen = match machine.last_seen {
        Some(seen) => format!("last seen {}", seen.local_ago(UnixTime::now())),
        None => "never connected".into(),
    };
    Refusal::new(
        Reason::NotReady,
        format!(
            "machine {} is offline ({seen}); run this once it is back",
            machine.name
        ),
    )
}
