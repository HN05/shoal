//! A machine's side of a request for a space another joined machine owns:
//! its daemon finds that machine in the hub's directory and forwards the
//! request there through the hub, vouching for its caller, the user or a
//! space's process, whose scope the owning machine checks, and reads the
//! answer of one that goes on, as a follow's, from a stream as it comes,
//! writing on it too what the caller sends the other way, as to a demo. A
//! request that needs the caller's machine is refused, naming the space's.
use crate::log;
use shoal_daemon_base::callers::{Access, Caller};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Member,
        directory::{Keeps, Listed},
        link::{Call, Reply},
        routing::{Delivery, Forwarded, Vouched, VouchedSpace},
    },
    protocol::{Body, Forwarding, Operation, Response, attaches_files},
    space_names::SpaceSelector,
    status::ReportedStatus,
};

use super::{
    session::{self, Calls},
    streams::{Incoming, Outgoing},
};
use crate::{
    Hub,
    daemon::Daemon,
    refusal::{Reason, Refusal},
};

/// A request for a space this machine has none of, as its caller sent it.
pub struct Unanswered<'a> {
    /// Its method's JSON; none for one that is never forwarded.
    pub method: Option<serde_json::Value>,
    pub forwarding: Forwarding,
    pub caller: Option<&'a Caller>,
    /// What of a removed space it acts on, which the machine that keeps
    /// that answers for the space; none for a live space alone.
    pub kept: Option<Keeps>,
}

/// How another machine answered a forwarded request.
pub enum Elsewhere {
    /// The frames its daemon wrote.
    Answered(Vec<serde_json::Value>),
    /// The hub holds it until the machine connects.
    Queued(shoal_protocol::hub::routing::Queued),
    /// What the offline machine last reported of the space, which answers
    /// a request that takes that.
    Reported(Box<ReportedStatus>),
    /// The frames its daemon writes come on this stream.
    Streamed(Incoming),
    /// Its daemon's answer comes on this duplex stream, followed by the
    /// bytes its connection carries, and the caller's go on its writer.
    Duplex(Incoming, Outgoing),
}

impl Hub {
    /// The answer to `request`, which named `space` that this machine has
    /// none of, from the other machine the hub lists that space on, or for
    /// a removed space's record or files, the one that keeps what it acts
    /// on; none when no other machine has it, so the request's own refusal
    /// stands.
    pub async fn answer_elsewhere(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
        request: Unanswered<'_>,
    ) -> Option<Result<Elsewhere>> {
        let (machine, listed) = match self.machine_of(daemon, space).await {
            Ok(Some(found)) => found,
            Ok(None) => {
                let keeps = request.kept?;
                return self.answer_kept(daemon, space, keeps, request).await;
            }
            // A space several machines have is refused as such.
            Err(error) if Reason::of(&error).is_some() => return Some(Err(error)),
            // A follow its client opens again, as after the hub restarted,
            // waits for the hub to answer.
            Err(error)
                if matches!(
                    request.forwarding,
                    Forwarding::Streamed | Forwarding::Duplex
                ) =>
            {
                return Some(Err(Refusal::wrap(Reason::MachineUnreachable, error)));
            }
            Err(error) => {
                log!("hub: could not find the machine of space {space}: {error:#}");
                return None;
            }
        };
        Some(
            self.forward(daemon, &machine, space, &listed, request)
                .await,
        )
    }

    /// The other joined machine whose space `selector` names, as the hub's
    /// directory lists it; none on a machine in no hub.
    pub async fn machine_of(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Option<(Member, Listed)>> {
        let Some(view) = self.hub_view(daemon, None).await? else {
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

    /// The answer to `request` for what `keeps` says of the removed space
    /// `space` names from the other machine that keeps it; none when no
    /// other machine does.
    async fn answer_kept(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
        keeps: Keeps,
        request: Unanswered<'_>,
    ) -> Option<Result<Elsewhere>> {
        let machine = match self.keeper_of(daemon, space, keeps).await {
            Ok(machine) => machine?,
            Err(error) => {
                let kept = keeps.as_str();
                log!(
                    "hub: could not find the machine that keeps the {kept} of space {space}: {error:#}"
                );
                return None;
            }
        };
        let forwarded = async {
            let method = without_caller_machine(forwardable(&request, space, &machine)?)?;
            let to = (space, space.given_id());
            (self.send(daemon, &machine, to, method, &request, Delivery::Now)).await
        };
        Some(forwarded.await)
    }

    /// The other joined machine that keeps what `keeps` says of the removed
    /// space `selector` names by its ID, as the hub's directory lists it.
    pub async fn keeper_of(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        keeps: Keeps,
    ) -> Result<Option<Member>> {
        let Some(id) = selector.given_id() else {
            return Ok(None);
        };
        let Some((own, directory)) = self.hub_directory(daemon).await? else {
            return Ok(None);
        };
        let mut kept = directory.kept.iter();
        let keeper =
            kept.find(|kept| &kept.space == id && kept.keeps == keeps && kept.machine != own);
        Ok(keeper.and_then(|kept| directory.machine(&kept.machine).cloned()))
    }

    /// Forward `request` for the space `space` to `machine`, unless it
    /// stays on that machine.
    async fn forward(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        space: &SpaceSelector,
        listed: &Listed,
        request: Unanswered<'_>,
    ) -> Result<Elsewhere> {
        let method = forwardable(&request, space, machine)?;
        if request.forwarding == Forwarding::Reported && !machine.connected {
            return self
                .reported(daemon, machine, space, listed, request.caller)
                .await
                .map(Elsewhere::Reported);
        }
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
        let to = (space, Some(&listed.space.id));
        self.send(daemon, machine, to, method, &request, delivery)
            .await
    }

    /// Send `method`, which `request` for the space `space`, with the ID
    /// `id` where it has one, became, to `machine` through the hub with
    /// `delivery`, vouching for its caller.
    async fn send(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        (space, id): (&SpaceSelector, Option<&SpaceId>),
        method: serde_json::Value,
        request: &Unanswered<'_>,
        delivery: Delivery,
    ) -> Result<Elsewhere> {
        let name = shoal_protocol::protocol::name_of(Ok(method.clone()));
        refuse_attached(
            &method,
            &format!("space {space} runs on machine {}", machine.name),
        )?;
        let forwarded = Forwarded {
            caller: self.vouch(daemon, request.caller).await?,
            method,
            summary: format!("{name} for space {space}"),
            space: id.cloned(),
            unheard: false,
        };
        if let Forwarding::Streamed | Forwarding::Duplex = request.forwarding {
            // Refusals of the request itself come on the stream, so what
            // fails here is reaching the machine.
            let duplex = request.forwarding == Forwarding::Duplex;
            return (self
                .stream_forwarded(daemon, machine, forwarded, duplex)
                .await)
                .map_err(|error| Refusal::wrap(Reason::MachineUnreachable, error));
        }
        let sent = self
            .send_forwarded(daemon, machine, forwarded, delivery)
            .await;
        let sent = match (sent, id) {
            (Err(error), Some(id)) if !machine.connected => {
                Err(self.naming_takeover(daemon, id, error).await)
            }
            (sent, _) => sent,
        };
        match sent.map_err(|error| unanswered(machine, space, error))? {
            Reply::Answered(answered) => Ok(Elsewhere::Answered(answered.frames)),
            Reply::Queued(queued) => Ok(Elsewhere::Queued(queued)),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// `error`, the refusal of a request for the space `id` whose machine
    /// is offline, naming `shoal restore`, which takes the space over here,
    /// where the hub keeps a package of it.
    async fn naming_takeover(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        error: anyhow::Error,
    ) -> anyhow::Error {
        match self.kept_package(daemon, id).await {
            Ok(Some(_)) => {
                let reason = Reason::of(&error).unwrap_or(Reason::NotReady);
                let message = format!("{error:#}, or take it over here with shoal restore {id}");
                Refusal::new(reason, message).into()
            }
            Ok(None) => error,
            Err(lookup) => {
                log!("hub: could not ask for a package of space {id}: {lookup:#}");
                error
            }
        }
    }

    /// What the offline `machine` last reported of the space `space` names,
    /// as the hub lists it in `listed`, for `caller`, whose scope must reach
    /// it, as the owning machine would check.
    async fn reported(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        space: &SpaceSelector,
        listed: &Listed,
        caller: Option<&Caller>,
    ) -> Result<Box<ReportedStatus>> {
        if let Some(caller) = caller
            && !self
                .reaches_elsewhere(daemon, caller, &listed.space.id)
                .await?
        {
            let message = format!("space {space} is outside this space's scope");
            return Err(Refusal::new(Reason::NotFound, message).into());
        }
        Ok(Box::new(ReportedStatus {
            space: listed.clone(),
            machine: machine.clone(),
        }))
    }

    /// The answer `machine` gives `operation` from `caller`, which this
    /// machine forwards there through the hub, leaving out what belongs to
    /// this machine; it fails at once while that machine is offline.
    pub async fn ask_machine(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        operation: Operation,
        caller: Option<&Caller>,
    ) -> Result<Body> {
        let request = self
            .forwarded_to(daemon, machine, operation, caller)
            .await?;
        match self
            .send_forwarded(daemon, machine, request, Delivery::Now)
            .await?
        {
            Reply::Answered(answered) => answered_body(answered.frames),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// The answer `machine` gives `operation` from `caller`, which this
    /// machine forwards there through the hub, leaving out what belongs to
    /// this machine; it fails with [`Queued`] while the hub holds it for
    /// that machine, which is offline, as messages and decisions wait.
    ///
    /// [`Queued`]: shoal_protocol::hub::routing::Queued
    pub async fn ask_machine_or_hold(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        operation: Operation,
        caller: Option<&Caller>,
    ) -> Result<Body> {
        let request = self
            .forwarded_to(daemon, machine, operation, caller)
            .await?;
        match self
            .send_forwarded(daemon, machine, request, Delivery::Queued)
            .await?
        {
            Reply::Answered(answered) => answered_body(answered.frames),
            Reply::Queued(queued) => Err(queued.into()),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// Have `machine` carry out `operation` for the user, through the hub,
    /// which holds it while that machine is offline, keeping it in this
    /// machine's outbox while the hub cannot be reached, so it goes once
    /// it can; an error that machine answered fails it, and one it kept or
    /// the hub holds succeeds, of which the user hears nothing should it
    /// expire or fail there, since it only keeps that machine in step.
    pub async fn tell_machine(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        operation: Operation,
    ) -> Result<()> {
        let request = Forwarded {
            unheard: true,
            ..self.forwarded_to(daemon, machine, operation, None).await?
        };
        let reply = if self.is_hub(daemon) {
            let own = self.own_machine(daemon).await?;
            let routed = self.route_request(daemon, &own, &machine.id, request, Delivery::Queued);
            Some(routed.await?)
        } else {
            let call = Call::Forward {
                machine: machine.id.clone(),
                request,
                delivery: Delivery::Queued,
            };
            self.call_or_hold(call).await?
        };
        match reply {
            Some(Reply::Answered(answered)) => answered_body(answered.frames).map(drop),
            Some(Reply::Queued(_)) | None => Ok(()),
            Some(reply) => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// The duplex stream `machine` answers `operation` from `caller` on,
    /// which this machine forwards there through the hub, with the stream
    /// that carries what this machine writes there; it fails at once while
    /// that machine is offline.
    pub async fn open_duplex(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        operation: Operation,
        caller: Option<&Caller>,
    ) -> Result<(Incoming, Outgoing)> {
        let request = self
            .forwarded_to(daemon, machine, operation, caller)
            .await?;
        match self
            .stream_forwarded(daemon, machine, request, true)
            .await?
        {
            Elsewhere::Duplex(incoming, outgoing) => Ok((incoming, outgoing)),
            _ => Err(anyhow!("the hub opened no duplex stream")),
        }
    }

    /// `operation` from `caller` as this machine forwards it to `machine`,
    /// without what belongs to this machine.
    async fn forwarded_to(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        mut operation: Operation,
        caller: Option<&Caller>,
    ) -> Result<Forwarded> {
        operation.leave_caller_machine();
        let method = serde_json::to_value(operation)?;
        refuse_attached(&method, &format!("machine {}", machine.name))?;
        let name = shoal_protocol::protocol::name_of(Ok(method.clone()));
        Ok(Forwarded {
            caller: self.vouch(daemon, caller).await?,
            method,
            summary: format!("{name} on machine {}", machine.name),
            space: None,
            unheard: false,
        })
    }

    /// Send `request` to `machine` through the hub: this daemon's own
    /// routing on the hub, else this machine's link to it.
    pub(super) async fn send_forwarded(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        request: Forwarded,
        delivery: Delivery,
    ) -> Result<Reply> {
        if self.is_hub(daemon) {
            let own = self.own_machine(daemon).await?;
            return (self.route_request(daemon, &own, &machine.id, request, delivery)).await;
        }
        let call = Call::Forward {
            machine: machine.id.clone(),
            request,
            delivery,
        };
        self.through_hub(call).await
    }

    /// The stream `machine` answers `request` on, through the hub, which
    /// also carries what the caller writes when `duplex`.
    async fn stream_forwarded(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        request: Forwarded,
        duplex: bool,
    ) -> Result<Elsewhere> {
        let (calls, call) = self.stream_route(daemon, machine, request).await?;
        Ok(match duplex {
            true => {
                let (incoming, outgoing) = calls.duplex(call).await?;
                Elsewhere::Duplex(incoming, outgoing)
            }
            false => Elsewhere::Streamed(calls.stream(call).await?),
        })
    }

    /// The link a stream that answers `request` goes on, with the call
    /// that asks `machine` for it on the stream's number: from the hub, the
    /// link to that machine; else, this machine's link to the hub, which
    /// passes it on. Either fails at once while that machine is offline.
    async fn stream_route(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        request: Forwarded,
    ) -> Result<(Arc<Calls>, StreamCall)> {
        if !self.is_hub(daemon) {
            let machine = machine.id.clone();
            let call = move |stream| Call::Stream {
                machine,
                request,
                stream,
            };
            return Ok((self.hub_calls().await?, Box::new(call)));
        }
        let machine = self.hub_machine(daemon, &machine.id).await?;
        let Some(calls) = self.links.calls(&machine.id) else {
            return Err(super::routing::offline(&machine).into());
        };
        let call = |stream| Call::ServeStream { request, stream };
        Ok((calls, Box::new(call)))
    }

    /// What this machine vouches for `caller`: the user, or a space's
    /// process with the reach of its scope and its effective permissions.
    async fn vouch(&self, daemon: &dyn Daemon, caller: Option<&Caller>) -> Result<Vouched> {
        let Some(caller) = caller else {
            return Ok(Vouched::User);
        };
        let everywhere = daemon.reach_root(Some(caller)).await?.is_none();
        let space = daemon.space(&caller.space_id).await?;
        let repository = space.repository_id.as_ref();
        let permissions = daemon.caller_permissions(caller, repository).await?;
        let chat_modes = daemon.chat_modes(&space).await?;
        let priority = daemon.priority(&caller.space_id).await?;
        Ok(Vouched::Space(VouchedSpace {
            space: caller.space_id.clone(),
            everywhere,
            permissions,
            granted: caller.granted,
            // A review's or answer's run writes only on its own machine.
            read_only: caller.access != Access::Full,
            priority,
            chat_modes,
        }))
    }
}

/// The answer `frames` carry, what another machine's daemon wrote after any
/// it sent while it still prepared it: one body, or a list's pages joined,
/// which a closing `Ok` ends; its refusal as an error.
fn answered_body(frames: Vec<serde_json::Value>) -> Result<Body> {
    let mut answered: Option<Body> = None;
    for frame in frames {
        let response: Response = serde_json::from_value(frame)?;
        match (&mut answered, response.body) {
            (_, Body::Keepalive) => {}
            (None, body) => answered = Some(body),
            (Some(_), Body::Ok) => break,
            (Some(answered), page) => answered.extend(page)?,
        }
    }
    answered
        .context("the other machine's daemon closed the request unanswered")?
        .into_result()
}

/// `error`, of a request for the space `space` that `machine` runs: where
/// the machine received the request but did not answer in time, a refusal
/// saying that it may still carry it out.
fn unanswered(machine: &Member, space: &SpaceSelector, error: anyhow::Error) -> anyhow::Error {
    if !error.is::<session::Unanswered>() {
        return error;
    }
    let message = format!(
        "machine {} did not answer in time ({error:#}); the request may go on there, which shoal status {space} shows",
        machine.name
    );
    Refusal::new(Reason::MachineUnreachable, message).into()
}

/// The method of `request` for the space `space`, which goes to `machine`;
/// refused for one that stays on the caller's machine.
fn forwardable(
    request: &Unanswered<'_>,
    space: &SpaceSelector,
    machine: &Member,
) -> Result<serde_json::Value> {
    match &request.method {
        Some(method) if request.forwarding != Forwarding::Never => Ok(method.clone()),
        _ => {
            let message = format!(
                "space {space} runs on machine {}, and this command acts only on the machine it runs on; run it there",
                machine.name
            );
            Err(Refusal::new(Reason::NotReady, message).into())
        }
    }
}

/// The call that asks for a stream, on its number.
type StreamCall = Box<dyn FnOnce(u64) -> Call + Send>;

/// The request `method` as it goes to the machine of `listed`: without
/// what belongs to this machine, such as an environment, which that
/// machine has its own of, and naming the item it acts on as the hub lists
/// it, where the caller named it by number alone; and whether it names
/// every item it acts on so.
fn prepared(method: serde_json::Value, listed: &Listed) -> Result<(serde_json::Value, bool)> {
    let mut operation = caller_machine_left(method)?;
    operation.pin_item(&listed.space.approvals);
    let pinned = !operation.names_unpinned_item();
    Ok((serde_json::to_value(operation)?, pinned))
}

/// The request `method` without what belongs to this machine.
fn without_caller_machine(method: serde_json::Value) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(caller_machine_left(method)?)?)
}

/// The operation `method` names, without what belongs to this machine.
fn caller_machine_left(method: serde_json::Value) -> Result<Operation> {
    let mut operation: Operation = serde_json::from_value(method)?;
    operation.leave_caller_machine();
    Ok(operation)
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
        // An alert's target, `{"to": {"space": {"space": ...}}}`.
        if let Some(target) = (fields.get_mut("to"))
            .and_then(|to| to.get_mut("space"))
            .and_then(serde_json::Value::as_object_mut)
        {
            bind_fields(target, &named, &bound);
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

/// Refuse the request `method` when it attaches files, as
/// [`files_stay_here`] refuses them.
fn refuse_attached(method: &serde_json::Value, elsewhere: &str) -> Result<()> {
    match attaches_files(method) {
        true => Err(files_stay_here(elsewhere)),
        false => Ok(()),
    }
}

/// The refusal of files attached here on what goes to `elsewhere`, which
/// names where it runs, as "machine desk": that machine cannot read this
/// one's files.
pub fn files_stay_here(elsewhere: &str) -> anyhow::Error {
    let message = format!(
        "{elsewhere}, which cannot read files attached here; send it without them, or from that machine"
    );
    Refusal::new(Reason::NotReady, message).into()
}

#[cfg(test)]
mod tests;
