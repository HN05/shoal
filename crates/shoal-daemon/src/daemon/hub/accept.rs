//! The hub's side of a member's link: it checks the member's protocol, then
//! proves its own key and checks the member's, and serves the link once it
//! takes the member: a new one with a pairing code, or through the daemon's
//! socket, without one; a paired one by the key it paired with, unless the
//! hub revoked it or another installation holds its link.
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    hub::{
        ExchangeKey, Mac, Member, PublicKey, Signature,
        link::{Frame, Hello, Intent, Joining, Refusal, RefusalReason, Signer, Transcript},
    },
    protocol,
};
use tokio::sync::OwnedRwLockReadGuard;

use super::{
    identity::{nonce, verify},
    links::Registration,
    members, pairing,
    sealing::Exchange,
    session::{self, Calls, End},
    transport::Channel,
};
use crate::daemon::{log, notifications::NotificationKind, workspace::Manager};

/// How long a member has to send its proof.
const PROOF_TIMEOUT: Duration = Duration::from_secs(30);

/// What a link reached the hub over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Over {
    /// The daemon's socket, which only the user's own processes reach, here
    /// or through a command such as `ssh`.
    Socket,
    /// The listener, which anyone who reaches its address does.
    Listener,
}

/// A member the hub took, with its open link.
struct Admitted<'a> {
    member: Member,
    version: String,
    registration: Registration<'a>,
}

impl Manager {
    /// Serve the link a member opened over `over` with `opening` in a request
    /// of `protocol`, until it ends, once the member and the hub proved
    /// their keys.
    pub(super) async fn accept_member(
        self: &Arc<Self>,
        mut channel: Channel,
        protocol: u32,
        opening: Value,
        over: Over,
    ) {
        match self.admit(&mut channel, protocol, opening, over).await {
            Ok(Ok(admitted)) => self.serve_member(channel, admitted).await,
            Ok(Err(refusal)) => {
                log!("hub: refused a link: {}", refusal.message);
                channel.close_with(&Frame::Refused(refusal)).await;
            }
            Err(error) => log!("hub: a link failed to open: {error:#}"),
        }
    }

    /// The member that opened `channel`, or why the hub refuses it. The
    /// link is sealed once the member proved its key, so every later
    /// refusal and the welcome go sealed.
    async fn admit(
        &self,
        channel: &mut Channel,
        protocol: u32,
        opening: Value,
        over: Over,
    ) -> Result<Result<Admitted<'_>, Refusal>> {
        if !self.is_hub() {
            return Ok(Err(refusal(
                RefusalReason::NotHub,
                "this daemon is not the hub; set [machine.hub] enabled there",
            )));
        }
        if protocol != protocol::VERSION {
            return Ok(Err(self.refuse_version(&opening, protocol).await));
        }
        let hello: Hello = serde_json::from_value(opening).context("unreadable hub link hello")?;
        let exchange = Exchange::new()?;
        let Some((proof, transcript)) = self.challenge(channel, &hello, &exchange.public).await?
        else {
            return Ok(Err(refusal(
                RefusalReason::Proof,
                "no proof of the member's key",
            )));
        };
        // A revocation commits and ends the member's link under the same
        // lock, so none slips between reading the member and registering
        // its link.
        let _admission = self.hub.admissions.lock().await;
        let claimant = match self.claimant(&hello.intent, &proof).await? {
            Ok(claimant) => claimant,
            Err(refusal) => return Ok(Err(refusal)),
        };
        channel
            .seal(exchange.keys(&transcript, Signer::Hub)?)
            .await?;
        let (member, _operation) = match self.take(claimant, &proof, over).await? {
            Ok(taken) => taken,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let Some(registration) = self.hub.links.register(&member.id, &hello.instance) else {
            return Ok(Err(refusal(
                RefusalReason::Duplicate,
                format!(
                    "another machine holds the link of {} {}, as one with a copy of its state directory does; run shoal hub join there to join as a machine of its own",
                    member.kind, member.name
                ),
            )));
        };
        let welcome = Frame::Welcome {
            member: member.id.clone(),
            name: member.name.clone(),
            hub: self.own_member().await?,
        };
        channel.sender().send_written(&welcome).await?;
        Ok(Ok(Admitted {
            member,
            version: hello.version,
            registration,
        }))
    }

    /// Prove the hub's key, with its half of the key exchange, `exchange`,
    /// to the member that sent `hello`, and read its proof of its own with
    /// the transcript both signed; none when it sends no proof in time.
    async fn challenge(
        &self,
        channel: &mut Channel,
        hello: &Hello,
        exchange: &ExchangeKey,
    ) -> Result<Option<(Proof, Transcript)>> {
        let identity = self.hub_identity().await?;
        let transcript = Transcript {
            hello: hello.clone(),
            hub_nonce: nonce(),
            hub_exchange: exchange.clone(),
        };
        let challenge = Frame::Challenge {
            key: identity.public.clone(),
            nonce: transcript.hub_nonce.clone(),
            exchange: exchange.clone(),
            signature: identity.sign(&transcript.signed(Signer::Hub)),
        };
        channel.send(&challenge).await?;
        let frame = tokio::time::timeout(PROOF_TIMEOUT, channel.expect::<Frame>()).await;
        match frame {
            Ok(Ok(Frame::Proof { signature, code })) => {
                let signed = transcript.signed(Signer::Member);
                let proof = Proof {
                    signed,
                    signature,
                    code,
                };
                Ok(Some((proof, transcript)))
            }
            Ok(Ok(frame)) => bail!("expected the member's proof, not {frame:?}"),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    /// Who `intent` claims the member is, once `proof` proves it holds the
    /// key that claim names, or why the hub refuses it.
    async fn claimant<'i>(
        &self,
        intent: &'i Intent,
        proof: &Proof,
    ) -> Result<Result<Claimant<'i>, Refusal>> {
        let claimant = match intent {
            Intent::Join(joining) => Claimant::New(joining),
            Intent::Connect { member } => {
                let own = self.hub_identity().await?.public.clone();
                let id = member.clone();
                let recorded = self.store.read(move |tx| members::find(tx, &id, &own));
                match recorded.await? {
                    Some(recorded) => Claimant::Paired(Box::new(recorded)),
                    None => {
                        return Ok(Err(refusal(
                            RefusalReason::Proof,
                            "the hub has no such member; run shoal hub join to join it again",
                        )));
                    }
                }
            }
        };
        if !proof.proves(claimant.key()) {
            return Ok(Err(refusal(
                RefusalReason::Proof,
                "the member's signature does not check",
            )));
        }
        Ok(Ok(claimant))
    }

    /// The member `claimant` becomes: a new one recorded once a pairing
    /// code admits it, or a link over the daemon's socket, with the daemon
    /// operation the record holds until the member is welcomed; a paired
    /// one unless the hub revoked it. Otherwise why the hub refuses it.
    async fn take(
        &self,
        claimant: Claimant<'_>,
        proof: &Proof,
        over: Over,
    ) -> Result<Result<(Member, Option<OwnedRwLockReadGuard<()>>), Refusal>> {
        let joining = match claimant {
            Claimant::Paired(recorded) if recorded.revoked => {
                return Ok(Err(refusal(
                    RefusalReason::Revoked,
                    format!(
                        "the hub revoked the credential of {} {}; run shoal hub join to join it again",
                        recorded.member.kind, recorded.member.name
                    ),
                )));
            }
            Claimant::Paired(recorded) => return Ok(Ok((recorded.member, None))),
            Claimant::New(joining) => joining,
        };
        let admitted =
            over == Over::Socket || self.hub.codes.take_proven(|code| proof.proves_code(code));
        if !admitted {
            return Ok(Err(refusal(
                RefusalReason::Code,
                "the pairing code is unknown, used or expired; run shoal hub pair for a new one",
            )));
        }
        let Some(operation) = self.gates.operation_unless_exiting().await else {
            return Ok(Err(refusal(
                RefusalReason::Restarting,
                "the hub's daemon is restarting; join again once it is back",
            )));
        };
        let member = self.join_member(joining).await?;
        Ok(Ok((member, Some(operation))))
    }

    /// Record the new member `joining` asks for.
    async fn join_member(&self, joining: &Joining) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let Joining {
            kind, name, key, ..
        } = joining.clone();
        self.store
            .write(move |tx| {
                let id = members::join(tx, kind, &name, &key)?;
                let recorded = members::find(tx, &id, &own)?;
                Ok(recorded.context("a member just recorded")?.member)
            })
            .await
    }

    /// The refusal of a member that speaks `protocol`, after recording an
    /// alert that names it and the upgrade it needs.
    async fn refuse_version(&self, opening: &Value, protocol: u32) -> Refusal {
        let ours = env!("CARGO_PKG_VERSION");
        let theirs = opening["version"].as_str().unwrap_or("of another release");
        let name = self.opening_name(opening).await;
        let upgrade = match newer(theirs, ours) {
            true => "run shoal update on this hub",
            false => "run shoal update on it",
        };
        let message = format!(
            "The hub refused {name}, which runs Shoal {theirs} (protocol {protocol}); this hub runs Shoal {ours} (protocol {}) and takes only that one: {upgrade}",
            protocol::VERSION
        );
        self.notify(None, NotificationKind::HubRefused, &message)
            .await;
        refusal(RefusalReason::Version, message)
    }

    /// What names the member that sent `opening`, as far as the hub can read
    /// a hello of another release.
    async fn opening_name(&self, opening: &Value) -> String {
        let intent = &opening["intent"];
        if let Some(name) = intent["join"]["name"].as_str() {
            return format!("the joining {}", name);
        }
        let Some(id) = intent["connect"]["member"].as_str() else {
            return "a machine or device".into();
        };
        let id = MemberId::from(id.to_owned());
        let Ok(own) = self
            .hub_identity()
            .await
            .map(|identity| identity.public.clone())
        else {
            return format!("member {id}");
        };
        let found = self.store.read({
            let id = id.clone();
            move |tx| members::find(tx, &id, &own)
        });
        match found.await {
            Ok(Some(recorded)) => format!("{} {}", recorded.member.kind, recorded.member.name),
            _ => format!("member {id}"),
        }
    }

    /// Serve an admitted member's link until it ends.
    async fn serve_member(self: &Arc<Self>, channel: Channel, admitted: Admitted<'_>) {
        let Admitted {
            member,
            version,
            registration,
        } = admitted;
        self.member_seen(&member.id, &version).await;
        log!("hub: {} {} connected", member.kind, member.name);
        let calls = Calls::new(channel.sender());
        let answer = |call| {
            let (manager, member) = (self.clone(), member.clone());
            async move { manager.answer_member(&member, call).await }
        };
        let end = session::serve(channel, &calls, answer, registration.stopped()).await;
        drop(registration);
        self.member_seen(&member.id, &version).await;
        match end {
            End::Closed | End::Stopped => log!("hub: {} {} disconnected", member.kind, member.name),
            End::Lost(error) => log!("hub: lost {} {}: {error:#}", member.kind, member.name),
        }
    }

    /// Record that `member` was seen now, running `version`.
    async fn member_seen(&self, member: &MemberId, version: &str) {
        let (member, version) = (member.clone(), version.to_owned());
        let seen = self
            .store
            .write(move |tx| members::seen(tx, &member, &version));
        if let Err(error) = seen.await {
            log!("hub: could not record a member's connection: {error:#}");
        }
    }
}

/// Who a member claims to be.
enum Claimant<'i> {
    New(&'i Joining),
    Paired(Box<members::Recorded>),
}

impl Claimant<'_> {
    /// The key the member must prove.
    fn key(&self) -> &PublicKey {
        match self {
            Self::New(joining) => &joining.key,
            Self::Paired(recorded) => &recorded.key,
        }
    }
}

/// A member's signature and what it must sign.
struct Proof {
    signed: Vec<u8>,
    signature: Signature,
    /// A joiner's proof of its pairing code.
    code: Option<Mac>,
}

impl Proof {
    fn proves(&self, key: &PublicKey) -> bool {
        verify(key, &self.signed, &self.signature)
    }

    fn proves_code(&self, code: &str) -> bool {
        (self.code.as_ref()).is_some_and(|proof| pairing::proves_code(code, &self.signed, proof))
    }
}

fn refusal(reason: RefusalReason, message: impl Into<String>) -> Refusal {
    Refusal {
        reason,
        message: message.into(),
    }
}

/// Whether the release `theirs` is newer than `ours`, comparing their
/// dotted numbers; a version that is not one counts as older.
fn newer(theirs: &str, ours: &str) -> bool {
    let numbers = |version: &str| -> Option<Vec<u64>> {
        version.split('.').map(|part| part.parse().ok()).collect()
    };
    match (numbers(theirs), numbers(ours)) {
        (Some(theirs), Some(ours)) => theirs > ours,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
