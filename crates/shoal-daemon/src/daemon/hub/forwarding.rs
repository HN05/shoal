//! A machine's side of a request for a space another joined machine owns:
//! its daemon finds that machine in the hub's directory and forwards the
//! request there through the hub, vouching for its caller, the user or a
//! space's process, whose scope the owning machine checks, and reads the
//! answer of one that goes on, as a follow's, from a stream as it comes. A
//! request that needs the space's files or processes is refused, naming the
//! machine.
use anyhow::{Result, anyhow};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Member,
        directory::Listed,
        link::{Call, Reply},
        routing::{Delivery, Forwarded, Vouched, VouchedSpace},
    },
    protocol::Forwarding,
    space_names::SpaceSelector,
};

use super::streams::Incoming;
use crate::{
    daemon::{manager::Manager, scope::Caller},
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
    /// The frames its daemon writes come on this stream.
    Streamed(Incoming),
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
        let (machine, listed) = match self.machine_of(space).await {
            Ok(Some(found)) => found,
            Ok(None) => return None,
            // A space several machines have is refused as such.
            Err(error) if Reason::of(&error).is_some() => return Some(Err(error)),
            // A follow its client opens again, as after the hub restarted,
            // waits for the hub to answer.
            Err(error) if request.forwarding == Forwarding::Streamed => {
                return Some(Err(Refusal::wrap(Reason::MachineUnreachable, error)));
            }
            Err(error) => {
                crate::daemon::log!("hub: could not find the machine of space {space}: {error:#}");
                return None;
            }
        };
        Some(self.forward(&machine, space, &listed, request).await)
    }

    /// The other joined machine whose space `selector` names, as the hub's
    /// directory lists it; none on a machine in no hub.
    pub(super) async fn machine_of(
        &self,
        selector: &SpaceSelector,
    ) -> Result<Option<(Member, Listed)>> {
        let Some(view) = self.hub_view(None).await? else {
            return Ok(None);
        };
        let directory = &view.directory;
        let mut listed = (directory.spaces.iter())
            .filter(|listed| selector.selects(&listed.space.id, listed.space.number));
        let listed = match (listed.next(), listed.next()) {
            (Some(listed), None) => listed,
            (None, _) => return Ok(None),
            // As while the hub renumbers one of them.
            (Some(_), Some(_)) => {
                let message = format!(
                    "several other machines have a space {selector}; name it by the ID shoal list --json shows"
                );
                return Err(Refusal::new(Reason::NotFound, message).into());
            }
        };
        let machine = directory.machine(&listed.machine).cloned();
        Ok(machine.map(|machine| (machine, listed.clone())))
    }

    /// Forward `request` for the space `space` to `machine`, unless it
    /// stays on that machine.
    async fn forward(
        &self,
        machine: &Member,
        space: &SpaceSelector,
        listed: &Listed,
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
        let (mut method, pinned) = prepared(method, listed)?;
        // An item named by number alone goes only while its machine is
        // there to read the number as it is meant now.
        let delivery = match pinned {
            true => delivery,
            false => Delivery::Now,
        };
        // A number another space there may take while it waits, never an ID.
        bind_space(&mut method, space, &listed.space.id)?;
        let name = shoal_protocol::protocol::name_of(Ok(method.clone()));
        if attaches_files(&method) {
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
        if request.forwarding == Forwarding::Streamed {
            // Refusals of the request itself come on the stream, so what
            // fails here is reaching the machine.
            let incoming = (self.stream_forwarded(machine, forwarded).await)
                .map_err(|error| Refusal::wrap(Reason::MachineUnreachable, error))?;
            return Ok(Elsewhere::Streamed(incoming));
        }
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

    /// The stream `machine` answers `request` on, through the hub: from the
    /// hub, the link to that machine; else, this machine's link to the hub,
    /// which passes it on. Either fails at once while that machine is
    /// offline.
    async fn stream_forwarded(&self, machine: &Member, request: Forwarded) -> Result<Incoming> {
        if !self.is_hub() {
            let call = |stream| Call::Stream {
                machine: machine.id.clone(),
                request,
                stream,
            };
            return self.hub_calls().await?.stream(call).await;
        }
        let machine = self.hub_machine(&machine.id).await?;
        let Some(calls) = self.hub.links.calls(&machine.id) else {
            return Err(super::routing::offline(&machine).into());
        };
        (calls.stream(|stream| Call::ServeStream { request, stream })).await
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
        let permissions = self.caller_permissions(caller, repository).await?;
        let chat_modes = self.chat_modes(&space).await?;
        let priority = self.space_priority(&caller.space_id).await?;
        Ok(Vouched::Space(VouchedSpace {
            space: caller.space_id.clone(),
            everywhere,
            permissions,
            granted: caller.granted,
            read_only: caller.read_only,
            priority,
            chat_modes,
        }))
    }
}

/// The request `method` as it goes to the machine of `listed`: without
/// what belongs to this machine, such as an environment, which that
/// machine has its own of, and naming the item it acts on as the hub lists
/// it, where the caller named it by number alone; and whether it names
/// every item it acts on so.
fn prepared(method: serde_json::Value, listed: &Listed) -> Result<(serde_json::Value, bool)> {
    let mut operation: shoal_protocol::protocol::Operation = serde_json::from_value(method)?;
    operation.leave_caller_machine();
    operation.pin_item(&listed.space.questions, &listed.space.approvals);
    let pinned = !operation.names_unpinned_item();
    Ok((serde_json::to_value(operation)?, pinned))
}

/// Name the space `selector` names in `method` by its ID `id`, in the
/// request's own space or workspace field, never in what it carries, such
/// as a workflow's inputs.
fn bind_space(
    method: &mut serde_json::Value,
    selector: &SpaceSelector,
    id: &SpaceId,
) -> Result<()> {
    let named = serde_json::to_value(selector)?;
    let bound = serde_json::to_value(SpaceSelector::id(id))?;
    let requests = method
        .as_object_mut()
        .into_iter()
        .flat_map(|request| request.values_mut());
    for fields in requests.filter_map(serde_json::Value::as_object_mut) {
        bind_fields(fields, &named, &bound);
        // A forge request's target, `{"space": {"space": ...}}`.
        if let Some(context) = fields
            .get_mut("space")
            .and_then(serde_json::Value::as_object_mut)
        {
            bind_fields(context, &named, &bound);
        }
    }
    Ok(())
}

/// Replace `named` with `bound` in the space and workspace fields of
/// `fields`.
fn bind_fields(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    named: &serde_json::Value,
    bound: &serde_json::Value,
) {
    for key in ["space", "workspace"] {
        if fields.get(key) == Some(named) {
            fields.insert(key.to_owned(), bound.clone());
        }
    }
}

/// Whether the request `method` attaches files by their path on this
/// machine, anywhere in it, as a message's do and an issue's or comment's
/// within its content.
fn attaches_files(method: &serde_json::Value) -> bool {
    match method {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            let files =
                key == "attachments" && value.as_array().is_some_and(|files| !files.is_empty());
            files || attaches_files(value)
        }),
        serde_json::Value::Array(items) => items.iter().any(attaches_files),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
