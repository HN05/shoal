//! A machine's side of a request for a space another joined machine owns:
//! its daemon finds that machine in the hub's directory and forwards the
//! request there through the hub, vouching for its caller, the user or a
//! space's process, whose scope the owning machine checks. A request that
//! needs the space's files or processes is refused, naming the machine.
use anyhow::{Result, anyhow};
use shoal_protocol::{
    hub::{
        Member,
        link::{Call, Reply},
        routing::{Delivery, Forwarded, Vouched, VouchedSpace},
    },
    protocol::Forwarding,
    space_names::SpaceSelector,
};

use crate::{
    daemon::{scope::Caller, workspace::Manager},
    refusal::{Reason, Refusal},
};

/// A request for a space this machine has none of, as its caller sent it.
pub(crate) struct Unanswered<'a> {
    /// Its method's JSON; none for one that is never forwarded.
    pub method: Option<serde_json::Value>,
    pub forwarding: Forwarding,
    pub caller: Option<&'a Caller>,
}

/// How another machine answered a forwarded request.
pub(crate) enum Elsewhere {
    /// The frames its daemon wrote.
    Answered(Vec<serde_json::Value>),
    /// The hub holds it until the machine connects.
    Queued(shoal_protocol::hub::routing::Queued),
}

impl Manager {
    /// The answer to `request`, which named `space` that this machine has
    /// none of, from the other machine the hub lists that space on; none
    /// when no other machine has it, so the request's own refusal stands.
    pub(crate) async fn answer_elsewhere(
        &self,
        space: &SpaceSelector,
        request: Unanswered<'_>,
    ) -> Option<Result<Elsewhere>> {
        let machine = match self.machine_of(space).await {
            Ok(Some(machine)) => machine,
            Ok(None) => return None,
            Err(error) => {
                crate::daemon::log!("hub: could not find the machine of space {space}: {error:#}");
                return None;
            }
        };
        Some(self.forward(&machine, space, request).await)
    }

    /// The other joined machine whose space `selector` names, as the hub's
    /// directory lists it; none on a machine in no hub.
    pub(super) async fn machine_of(&self, selector: &SpaceSelector) -> Result<Option<Member>> {
        let Some(view) = self.hub_view(None).await? else {
            return Ok(None);
        };
        let directory = &view.directory;
        let listed = (directory.spaces.iter())
            .find(|listed| selector.selects(&listed.space.id, listed.space.number));
        Ok(listed.and_then(|listed| directory.machine(&listed.machine).cloned()))
    }

    /// Forward `request` for the space `space` to `machine`, unless it
    /// stays on that machine.
    async fn forward(
        &self,
        machine: &Member,
        space: &SpaceSelector,
        request: Unanswered<'_>,
    ) -> Result<Elsewhere> {
        let Some(method) = request
            .method
            .filter(|_| request.forwarding != Forwarding::Never)
        else {
            let message = format!(
                "space {space} runs on machine {}, and this command needs its files or processes; run it there",
                machine.name
            );
            return Err(Refusal::new(Reason::NotReady, message).into());
        };
        let delivery = match request.forwarding {
            Forwarding::Queued => Delivery::Queued,
            _ => Delivery::Now,
        };
        let name = shoal_protocol::protocol::name_of(Ok(method.clone()));
        if attaches_files(&method, &name) {
            let message = format!(
                "space {space} runs on machine {}, which cannot read files attached here; send it without them, or from that machine",
                machine.name
            );
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        let forwarded = Forwarded {
            caller: self.vouch(request.caller).await?,
            method,
            summary: format!("{name} for space {space}"),
        };
        match self.send_forwarded(machine, forwarded, delivery).await? {
            Reply::Answered(answered) => Ok(Elsewhere::Answered(answered.frames)),
            Reply::Queued(queued) => Ok(Elsewhere::Queued(queued)),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// Send `request` to `machine` through the hub: this daemon's own
    /// routing on the hub, else this machine's link to it.
    pub(super) async fn send_forwarded(
        &self,
        machine: &Member,
        request: Forwarded,
        delivery: Delivery,
    ) -> Result<Reply> {
        if self.is_hub() {
            let own = self.own_machine().await?;
            return (self.route_request(&own, &machine.id, request, delivery)).await;
        }
        let call = Call::Forward {
            machine: machine.id.clone(),
            request,
            delivery,
        };
        self.through_hub(call).await
    }

    /// What this machine vouches for `caller`: the user, or a space's
    /// process with the reach of its scope and its effective permissions.
    async fn vouch(&self, caller: Option<&Caller>) -> Result<Vouched> {
        let Some(caller) = caller else {
            return Ok(Vouched::User);
        };
        let everywhere = self.reach_root(Some(caller)).await?.is_none();
        let space = self.space_by_id(&caller.space_id).await?;
        let repository = space.repository_id.as_ref();
        let permissions = self.space_permissions(&caller.space_id, repository).await?;
        let chat_modes = self.chat_modes(&space).await?;
        Ok(Vouched::Space(VouchedSpace {
            space: caller.space_id.clone(),
            everywhere,
            permissions,
            granted: caller.granted,
            read_only: caller.read_only,
            chat_modes,
        }))
    }
}

/// Whether the request `method`, named `name`, attaches files by their path
/// on this machine.
fn attaches_files(method: &serde_json::Value, name: &str) -> bool {
    (method[name]["attachments"].as_array()).is_some_and(|files| !files.is_empty())
}

#[cfg(test)]
mod tests;
