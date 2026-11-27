//! What the hub answers, to its members' calls and to its own machine's
//! callers alike.
use anyhow::{Result, anyhow};
use shoal_protocol::hub::{
    Member, MemberKind, Overview, Revoked,
    link::{Call, Reply},
    pairing::Pairing,
    sleep::Clocks,
};

use std::sync::Arc;

use shoal_ids::member_ids::MemberId;
use tokio::sync::OwnedRwLockReadGuard;

use super::{
    alerts, directory, members,
    packages::{self, Continuation, Sent, takeovers},
    session::Calls,
};
use crate::{
    Hub,
    daemon::Daemon,
    refusal::{Reason, Refusal},
    time::UnixTime,
};

/// What an answer holds until its reply is written. It holds the daemon
/// operation, so an update never exits between a change and the reply that
/// reports it. When the call revoked the caller's own credential, it also
/// holds the end of its link until the member has read that reply.
pub(super) struct Held {
    _operation: Option<OwnedRwLockReadGuard<()>>,
    ends: Option<(Arc<Hub>, MemberId)>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some((hub, member)) = self.ends.take() {
            hub.links.stop(&member);
        }
    }
}

impl Hub {
    /// The hub's reply to `member`'s `call`, with what it holds until the
    /// reply is written.
    pub(super) async fn answer_member(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        member: &Member,
        call: Call,
        calls: &Calls,
    ) -> (Reply, Held) {
        let Some(operation) = self.gates.operation_unless_exiting().await else {
            let message = "the hub's daemon is restarting; try again once it is back".into();
            let held = Held {
                _operation: None,
                ends: None,
            };
            return (Reply::Failed { message }, held);
        };
        let reply = super::session::reply(self.answer_call(daemon, member, call, calls).await);
        let revoked_itself = match &reply {
            Reply::Left => true,
            Reply::Revoked(revoked) => revoked.member.id == member.id,
            _ => false,
        };
        let held = Held {
            _operation: Some(operation),
            ends: revoked_itself.then(|| (self.clone(), member.id.clone())),
        };
        (reply, held)
    }

    /// The hub's reply to `member`'s `call`, refused once the hub revoked
    /// the member, whose link may still be closing. A stream it asks for
    /// goes on its link's `calls`.
    async fn answer_call(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        member: &Member,
        call: Call,
        calls: &Calls,
    ) -> Result<Reply> {
        self.ensure_hub(&**daemon)?;
        self.ensure_not_revoked(&member.id).await?;
        Ok(match call {
            Call::Pair => Reply::Pairing(self.issue_pairing(&**daemon).await?),
            Call::Members => Reply::Members(self.local_hub_overview(&**daemon).await?),
            Call::Revoke { member: selector } => {
                Reply::Revoked(self.revoke_member(&selector, Some(&member.id)).await?)
            }
            Call::Leave => {
                self.release_member(&member.id).await?;
                Reply::Left
            }
            Call::Update => Reply::Updated(self.update_own_installation().await?),
            Call::Report {
                report,
                reserved,
                kept,
                capacity,
                repositories,
                power,
                sent_at,
            } => {
                ensure_machine(member)?;
                let report = report.on_hub_clock(arrived(sent_at));
                self.sleeps.powered(&member.id, power);
                self.capacities.keep(&member.id, &report, capacity);
                let held = directory::Held { reserved, kept };
                let held = (held, repositories);
                Reply::Reported(
                    self.record_report(&**daemon, &member.id, report, held)
                        .await?,
                )
            }
            Call::ReportAlerts { report } => {
                ensure_machine(member)?;
                Reply::AlertsReported(self.record_alert_report(&member.id, report).await?)
            }
            Call::MarkAlerts { machine, marks } => {
                ensure_machine(member)?;
                self.take_alert_marks(&machine, marks).await?;
                Reply::Delivered
            }
            Call::FollowAlerts { stream } => {
                let (out, _) = calls.streams().answering(stream, false);
                self.serve_alert_copies(member.id.clone(), out);
                Reply::Streaming
            }
            Call::Claim {
                space,
                ticket,
                takeover,
            } => {
                ensure_machine(member)?;
                let claim = (ticket.as_ref(), takeover);
                Reply::Claimed(self.record_space_claim(&member.id, &space, claim).await?)
            }
            Call::Settle {
                space,
                machine,
                ticket,
            } => {
                ensure_machine(member)?;
                Reply::Settled(self.record_settled_move(&space, &machine, &ticket).await?)
            }
            Call::Describe { description } => {
                ensure_machine(member)?;
                Reply::Availability(self.describe_member(&member.id, description).await?)
            }
            Call::Directory => Reply::Directory(self.local_directory(&**daemon).await?),
            Call::Repositories => Reply::Directory(self.local_repositories(&**daemon).await?),
            Call::Place { mut request } => {
                ensure_machine(member)?;
                request.caller = member.id.clone();
                Reply::Placed(self.place_for(&**daemon, request).await?)
            }
            Call::Owner { space } => Reply::Owner(
                self.local_space_owner(&**daemon, &space, &member.id)
                    .await?,
            ),
            Call::Forward {
                machine,
                request,
                delivery,
            } => (self.route_request(&**daemon, member, &machine, request, delivery)).await?,
            Call::Stream {
                machine,
                request,
                stream,
            } => {
                let answering = (calls.streams(), stream);
                (self.route_stream(daemon, member, &machine, request, answering)).await?
            }
            Call::Tell { told } => {
                ensure_machine(member)?;
                self.route_told(&**daemon, &member.id, told).await?
            }
            Call::App {
                method,
                data,
                stream,
            } => {
                let (out, _) = calls.streams().answering(stream, false);
                daemon
                    .clone()
                    .serve_app_request(member, method, data, out)?;
                Reply::Streaming
            }
            Call::Keep {
                space,
                built_at,
                stream,
                start,
                within,
            } => {
                ensure_machine(member)?;
                let sent = Sent {
                    member: member.id.clone(),
                    space,
                    built_at,
                    arrived_at: UnixTime::now(),
                    continuation: Continuation { start, within },
                };
                let streams = (calls.streams(), stream);
                self.keep_package(&**daemon, sent, streams).await?;
                Reply::Streaming
            }
            Call::HandOver {
                space,
                built_at,
                stream,
                start,
                within,
            } => {
                ensure_machine(member)?;
                let sent = Sent {
                    member: member.id.clone(),
                    space,
                    built_at,
                    arrived_at: UnixTime::now(),
                    continuation: Continuation {
                        start: Some(start),
                        within,
                    },
                };
                let streams = (calls.streams(), stream);
                self.take_handover(daemon, sent, streams).await?;
                Reply::Streaming
            }
            Call::Release { space } => {
                ensure_machine(member)?;
                self.hear_release(daemon, &member.id, &space)?;
                Reply::Delivered
            }
            Call::Sleeping { lagging, sent_at } => {
                ensure_machine(member)?;
                let clocks = arrived(sent_at);
                let lagging = (lagging.into_iter())
                    .map(|lag| lag.on_hub_clock(clocks))
                    .collect();
                self.hear_sleep(&member.id, lagging).await?;
                Reply::Delivered
            }
            Call::CutOff { jobs, sent_at } => {
                ensure_machine(member)?;
                let clocks = arrived(sent_at);
                let jobs = (jobs.into_iter())
                    .map(|job| job.on_hub_clock(clocks))
                    .collect();
                self.hear_cut_off(&member.id, jobs).await?;
                Reply::Delivered
            }
            Call::Stay => {
                ensure_machine(member)?;
                self.hear_stay(&member.id);
                Reply::Delivered
            }
            Call::Kept { space } => {
                ensure_machine(member)?;
                Reply::Kept(self.local_kept_package(&space).await?.map(Box::new))
            }
            Call::Fetch { space, stream } => {
                ensure_machine(member)?;
                (self.serve_fetch(&member.id, &space, (calls.streams(), stream))).await?;
                Reply::Streaming
            }
            Call::Share { edits } => {
                ensure_machine(member)?;
                Reply::Shared(self.take_edits(daemon, &member.id, edits).await?)
            }
            Call::Value {
                machine,
                setting,
                command,
            } => {
                ensure_machine(member)?;
                let from = super::values::Requester {
                    id: &member.id,
                    name: &member.name,
                };
                let asked = super::values::Asked {
                    machine: &machine,
                    setting: &setting,
                    command: &command,
                };
                self.route_value(from, asked).await?
            }
            Call::Serve { .. }
            | Call::RunValue { .. }
            | Call::ServeStream { .. }
            | Call::Expired { .. }
            | Call::Availability { .. }
            | Call::Retire { .. }
            | Call::Continue { .. }
            | Call::RerunCutOff { .. }
            | Call::AlertsMarked { .. }
            | Call::Shared { .. } => {
                anyhow::bail!(
                    "only the hub passes requests, expiries, availability, retirements, hand-overs, reruns, shared items and value commands to machines"
                )
            }
        })
    }

    /// Refuse a member's call once this daemon no longer takes the hub
    /// role, whose links are then closing.
    fn ensure_hub(&self, daemon: &dyn Daemon) -> Result<()> {
        anyhow::ensure!(
            self.is_hub(daemon,),
            "this daemon no longer takes the hub role"
        );
        Ok(())
    }

    /// Refuse a call of the member `id` once the hub revoked it.
    async fn ensure_not_revoked(&self, id: &MemberId) -> Result<()> {
        let own = self.hub_identity().await?.public.clone();
        let id = id.clone();
        let recorded = self.store.read(move |tx| members::find(tx, &id, &own));
        match recorded.await? {
            Some(recorded) if !recorded.revoked => Ok(()),
            _ => Err(anyhow!("the hub revoked this machine's credential")),
        }
    }

    /// The listener and members of this daemon as the hub.
    pub(super) async fn local_hub_overview(&self, daemon: &dyn Daemon) -> Result<Overview> {
        self.own_member().await?;
        let own = self.hub_identity().await?.public.clone();
        let mut members = self.store.read(move |tx| members::list(tx, &own)).await?;
        let connected = self.links.connected();
        for member in &mut members {
            member.connected = member.hub || connected.contains(&member.id);
            if member.hub {
                member.version = Some(env!("CARGO_PKG_VERSION").into());
                member.last_seen = None;
            }
        }
        self.with_availability(&mut members).await?;
        self.with_capacity(&mut members);
        Ok(Overview {
            listener: self.hub_listener(daemon),
            members,
        })
    }

    /// Revoke the credential of the member `selector` names, and end its link.
    /// The `caller`'s own link ends only once it has read the reply. The reply
    /// names the settings the member fetched from other machines.
    pub(super) async fn revoke_member(
        &self,
        selector: &str,
        caller: Option<&MemberId>,
    ) -> Result<Revoked> {
        let own = self.hub_identity().await?.public.clone();
        let selector = selector.to_owned();
        let _admission = self.admissions.lock().await;
        let revoked = self
            .store
            .write(move |tx| {
                let member = members::select(tx, &selector, &own)?;
                if member.hub {
                    return Err(Refusal::new(
                        Reason::NotReady,
                        "the hub's own machine holds no credential; set [machine.hub] enabled = false to end the hub role",
                    )
                    .into());
                }
                members::revoke(tx, &member.id)?;
                directory::forget(tx, &member.id)?;
                alerts::forget(tx, &member.id)?;
                super::queue::forget(tx, &member.id)?;
                takeovers::forget(tx, &member.id)?;
                packages::forget_sleep(tx, &member.id)?;
                packages::forget(tx, &member.id)?;
                let fetched = super::value_fetches::of(tx, &member.id)?;
                Ok(Revoked { member, fetched })
            })
            .await?;
        if caller != Some(&revoked.member.id) {
            self.links.stop(&revoked.member.id);
        }
        self.alerts_dropped(&revoked.member.id);
        self.delete_ended_packages().await;
        Ok(revoked)
    }

    /// Revoke the credential of `member`, which leaves: its link stays
    /// open for the reply, and the member then closes it.
    async fn release_member(&self, id: &MemberId) -> Result<()> {
        let member = id.clone();
        let _admission = self.admissions.lock().await;
        self.store
            .write(move |tx| {
                members::revoke(tx, &member)?;
                directory::forget(tx, &member)?;
                alerts::forget(tx, &member)?;
                super::queue::forget(tx, &member)?;
                takeovers::forget(tx, &member)?;
                packages::forget_sleep(tx, &member)?;
                packages::forget(tx, &member)
            })
            .await?;
        self.alerts_dropped(id);
        self.delete_ended_packages().await;
        Ok(())
    }

    /// A pairing code, from the hub itself or through this machine's link.
    pub async fn hub_pairing(&self, daemon: &dyn Daemon) -> Result<Pairing> {
        if self.is_hub(daemon) {
            return self.issue_pairing(daemon).await;
        }
        match self.through_hub(Call::Pair).await? {
            Reply::Pairing(pairing) => Ok(pairing),
            reply => Err(unexpected(reply)),
        }
    }

    /// Revoke the member `selector` names, on the hub itself or through
    /// this machine's link.
    pub async fn revoke_hub_member(
        &self,
        daemon: &dyn Daemon,
        selector: String,
    ) -> Result<Revoked> {
        if self.is_hub(daemon) {
            return self.revoke_member(&selector, None).await;
        }
        match self.through_hub(Call::Revoke { member: selector }).await? {
            Reply::Revoked(revoked) => Ok(revoked),
            reply => Err(unexpected(reply)),
        }
    }
}

/// The clocks of a call a machine sent at `sent_at` by its own clock,
/// which arrives now by the hub's.
fn arrived(sent_at: UnixTime) -> Clocks {
    Clocks {
        sent: sent_at,
        received: UnixTime::now(),
    }
}

/// Refuse a call only a machine makes, such as a report of its spaces or
/// an event one passes up, from a device.
fn ensure_machine(member: &Member) -> Result<()> {
    anyhow::ensure!(
        member.kind == MemberKind::Machine,
        "only a machine reports, claims, keeps and passes up spaces, shares files and asks for values at the hub"
    );
    Ok(())
}

/// The overview a [`Call::Members`] reply carries.
pub(crate) fn overview(reply: Reply) -> Result<Overview> {
    match reply {
        Reply::Members(overview) => Ok(overview),
        reply => Err(unexpected(reply)),
    }
}

pub(super) fn unexpected(reply: Reply) -> anyhow::Error {
    anyhow!("the hub answered with an unexpected {reply:?}")
}
