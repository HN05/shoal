//! The hub's side of a member's link: it checks the member's protocol, then
//! proves its own key and checks the member's, and serves the link once it
//! takes the member: a new one with a pairing code, or through the daemon's
//! socket, without one; a paired one by the key it paired with, unless the
//! hub revoked it or another installation holds its link.
use shoal_daemon_base::notifications::About;
use shoal_protocol::alerts::AlertKind;
use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    hub::{
        ExchangeKey, Mac, Member, Nonce, PublicKey, Signature,
        link::{
            Frame, Hello, HubVersion, Intent, Joining, Refusal, RefusalReason, Signer, Transcript,
            Updates,
        },
    },
    protocol,
};
use tokio::{sync::OwnedRwLockReadGuard, time::Instant};

use super::{
    identity::{nonce, verify},
    links::Registration,
    members, pairing, presence,
    sealing::Exchange,
    session::{self, Calls, End},
    shown,
    transport::Channel,
};
use crate::{Hub, daemon::Daemon, log, time::UnixTime};

/// The only Shoal version the hub takes.
const OWN_VERSION: &str = env!("CARGO_PKG_VERSION");

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

impl Hub {
    /// Serve the link a member opened over `over` with `opening` in a request
    /// of `protocol`, until it ends, once the member and the hub proved
    /// their keys. From the member's proof until its welcome or refusal is
    /// written, admission holds a daemon operation, so an update never exits
    /// partway; the handshake before it, which waits on the member, holds
    /// none, and nor does the link it then serves.
    pub(super) async fn accept_member(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        mut channel: Channel,
        protocol: u32,
        opening: Value,
        over: Over,
    ) {
        let proven = match self
            .handshake(&**daemon, &mut channel, protocol, opening)
            .await
        {
            Ok(Ok(proven)) => proven,
            Ok(Err(refusal)) => {
                let operation = self.gates.operation_unless_exiting().await;
                return self.refuse(channel, refusal, operation).await;
            }
            Err(error) => return log_failed(&error),
        };
        let Some(operation) = self.gates.operation_unless_exiting().await else {
            let refusal = refusal(
                RefusalReason::Restarting,
                "the hub's daemon is restarting; connect again once it is back",
            );
            return self.refuse(channel, refusal, None).await;
        };
        match self.admit(&**daemon, &mut channel, proven, over).await {
            Ok(Ok(admitted)) => {
                drop(operation);
                self.serve_member(daemon, channel, admitted).await
            }
            Ok(Err(refusal)) => self.refuse(channel, refusal, Some(operation)).await,
            Err(error) => log_failed(&error),
        }
    }

    /// Send `refusal` and close `channel`, holding `operation`, if any,
    /// until it went.
    async fn refuse(
        &self,
        channel: Channel,
        refusal: Refusal,
        operation: Option<OwnedRwLockReadGuard<()>>,
    ) {
        log!("hub: refused a link: {}", refusal.message);
        channel.close_with(&Frame::Refused(refusal)).await;
        drop(operation);
    }

    /// The hello on `channel`, with the member's proof of its key, once the
    /// hub proved its own, or why the hub refuses it first.
    async fn handshake(
        &self,
        daemon: &dyn Daemon,
        channel: &mut Channel,
        protocol: u32,
        opening: Value,
    ) -> Result<Result<Proven, Refusal>> {
        if !self.is_hub(daemon) {
            return Ok(Err(not_hub()));
        }
        if !same_release(protocol, &opening) {
            return Ok(Err(self.refuse_version(daemon, &opening, protocol).await));
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
        Ok(Ok(Proven {
            hello,
            exchange,
            proof,
            transcript,
        }))
    }

    /// The member that `proven` proved, with its link registered once it
    /// was welcomed, or why the hub refuses it. The link is sealed once the
    /// hub knows whose key the proof checks with, so every later refusal
    /// and the welcome go sealed.
    async fn admit(
        &self,
        daemon: &dyn Daemon,
        channel: &mut Channel,
        proven: Proven,
        over: Over,
    ) -> Result<Result<Admitted<'_>, Refusal>> {
        let Proven {
            hello,
            exchange,
            proof,
            transcript,
        } = proven;
        // A revocation, and the end of the hub role, ends the member's links
        // under the same lock, so none slips between reading the member and
        // registering its link.
        let admission = self.admissions.lock().await;
        if !self.is_hub(daemon) {
            return Ok(Err(not_hub()));
        }
        let claimant = match self.claimant(&hello.intent, &proof).await? {
            Ok(claimant) => claimant,
            Err(refusal) => return Ok(Err(refusal)),
        };
        channel
            .seal(exchange.keys(&transcript, Signer::Hub)?)
            .await?;
        let member = match self.take(claimant, &proof, over).await? {
            Ok(member) => member,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let Some(registration) = self.links.register(&member.id, &hello.instance) else {
            return Ok(Err(refusal(
                RefusalReason::Duplicate,
                format!(
                    "another machine holds the link of {} {}, as one with a copy of its state directory does; run shoal hub join there to join as a machine of its own",
                    member.kind, member.name
                ),
            )));
        };
        // Registered: a revocation now ends this link, so writing the
        // welcome, however long the network takes, holds up no other.
        drop(admission);
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
            Ok(Ok(frame)) => bail!(
                "expected the member's proof, not {}",
                shown(&format!("{frame:?}"))
            ),
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
    /// code admits it, or a link over the daemon's socket; a paired one
    /// unless the hub revoked it. Otherwise why the hub refuses it.
    async fn take(
        &self,
        claimant: Claimant<'_>,
        proof: &Proof,
        over: Over,
    ) -> Result<Result<Member, Refusal>> {
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
            Claimant::Paired(recorded) => return Ok(Ok(recorded.member)),
            Claimant::New(joining) => joining,
        };
        let admitted =
            over == Over::Socket || self.codes.take_proven(|code| proof.proves_code(code));
        if !admitted {
            return Ok(Err(refusal(
                RefusalReason::Code,
                "the pairing code is unknown, used or expired; run shoal hub pair for a new one",
            )));
        }
        self.join_member(joining).await.map(Ok)
    }

    /// Record the new member `joining` asks for.
    async fn join_member(&self, joining: &Joining) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let Joining { kind, name, key } = joining.clone();
        // The joiner chose it, and logs and listings show it.
        let name = shown(&name);
        self.store
            .write(move |tx| {
                let id = members::join(tx, kind, &name, &key)?;
                let recorded = members::find(tx, &id, &own)?;
                Ok(recorded.context("a member just recorded")?.member)
            })
            .await
    }

    /// The refusal of a member that runs another version or speaks
    /// `protocol`, with the hub's version signed over its nonce, after
    /// recording an alert that names it, its version and the update it
    /// needs, for one that follows the hub only once it has not for
    /// [`FOLLOW_GRACE`]. What a peer sends here is not yet authenticated, so the alert
    /// names only a member the hub recorded, holds the version it claims
    /// short and printable, and comes at most once an hour for each member,
    /// or for all joiners together.
    async fn refuse_version(&self, daemon: &dyn Daemon, opening: &Value, protocol: u32) -> Refusal {
        let theirs = claimed_version(opening);
        let (alerted, name) = self.refused_member(opening).await;
        let newer = shoal_core::update::is_newer(&theirs, OWN_VERSION)
            .unwrap_or(protocol > protocol::VERSION);
        let updates = serde_json::from_value(opening["updates"].clone()).unwrap_or(Updates::Manual);
        let Advice { update, grace } = advice(updates, newer);
        let message = format!(
            "The hub refused {name}, which runs Shoal {theirs} (protocol {protocol}); this hub runs Shoal {OWN_VERSION} (protocol {}) and takes only that one: {update}",
            protocol::VERSION
        );
        if self.refusal_alerts.due(alerted, grace) {
            self.notify(
                daemon,
                About::named(None),
                AlertKind::HubRefused,
                message.to_string(),
            )
            .await;
        }
        Refusal {
            version: self.signed_version(opening).await,
            ..refusal(RefusalReason::Version, message)
        }
    }

    /// The hub's version signed over the nonce of the hello `opening`, none
    /// when it carries none the hub reads.
    async fn signed_version(&self, opening: &Value) -> Option<HubVersion> {
        let nonce: Nonce = serde_json::from_value(opening["nonce"].clone()).ok()?;
        let identity = self.hub_identity().await.ok()?;
        let schema = shoal_store::SCHEMA_VERSION;
        Some(HubVersion {
            version: OWN_VERSION.to_owned(),
            schema,
            key: identity.public.clone(),
            signature: identity.sign(&HubVersion::signed(OWN_VERSION, schema, &nonce)),
        })
    }

    /// Who an alert about the refused `opening` is for, and how it names
    /// them: a member the hub recorded, or any machine or device joining.
    async fn refused_member(&self, opening: &Value) -> (Option<MemberId>, String) {
        let joining = (None, "a machine or device joining".to_owned());
        let Some(id) = opening["intent"]["connect"]["member"].as_str() else {
            return joining;
        };
        let id = MemberId::from(id.to_owned());
        let Ok(own) = (self.hub_identity().await).map(|identity| identity.public.clone()) else {
            return joining;
        };
        let found = self.store.read({
            let id = id.clone();
            move |tx| members::find(tx, &id, &own)
        });
        match found.await {
            Ok(Some(recorded)) => {
                let name = format!("{} {}", recorded.member.kind, recorded.member.name);
                (Some(id), name)
            }
            _ => joining,
        }
    }

    /// Serve an admitted member's link until it ends.
    async fn serve_member(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        channel: Channel,
        admitted: Admitted<'_>,
    ) {
        let Admitted {
            member,
            version,
            registration,
        } = admitted;
        self.member_seen(&member.id, &version).await;
        self.refusal_alerts.taken(&member.id);
        log!("hub: {} {} connected", member.kind, member.name);
        let calls = Calls::new(channel.sender(), format!("{} {}", member.kind, member.name));
        registration.serve_calls(calls.clone());
        tokio::spawn((self.clone()).keep_delivering(
            daemon.clone(),
            member.id.clone(),
            calls.clone(),
        ));
        self.handovers.stays(&member.id);
        tokio::spawn((self.clone()).retire_taken_over(member.id.clone(), calls.clone()));
        let answer = |call| {
            let (hub, daemon, member, calls) =
                (self.clone(), daemon.clone(), member.clone(), calls.clone());
            async move { hub.answer_member(&daemon, &member, call, &calls).await }
        };
        let end = session::serve(channel, &calls, answer, registration.stopped()).await;
        drop(registration);
        self.member_seen(&member.id, &version).await;
        match end {
            End::Closed | End::Stopped => log!("hub: {} {} disconnected", member.kind, member.name),
            End::Lost(error) => {
                let error = shown(&format!("{error:#}"));
                log!("hub: lost {} {}: {error}", member.kind, member.name)
            }
        }
    }

    /// Record that `member` was seen, and was there, now, running
    /// `version`.
    async fn member_seen(&self, member: &MemberId, version: &str) {
        let (member, version) = (member.clone(), version.to_owned());
        let seen = self.store.write(move |tx| {
            members::seen(tx, &member, &version)?;
            presence::mark(tx, &member, UnixTime::now())
        });
        if let Err(error) = seen.await {
            log!("hub: could not record a member's connection: {error:#}");
        }
    }
}

/// How often the hub alerts the user to refusing the same member, or any
/// joiner, for another version.
const ALERT_EVERY: Duration = Duration::from_secs(60 * 60);

/// How long the hub refuses a member that follows its version before it
/// alerts the user: it installs that version meanwhile, and its daemon
/// restarts on it once idle.
const FOLLOW_GRACE: Duration = Duration::from_secs(30 * 60);

/// The update the hub's alert names for a member that `updates` as it
/// does, `newer` than the hub or not, and how long it waits first.
#[derive(Debug, PartialEq, Eq)]
struct Advice {
    update: String,
    grace: Duration,
}

fn advice(updates: Updates, newer: bool) -> Advice {
    let advice = |update: String, grace| Advice { update, grace };
    match (updates, newer) {
        // It installs an older release only where that reads its state.
        (Updates::Follows, true) => advice("run shoal hub update".into(), FOLLOW_GRACE),
        (Updates::Follows, false) => advice(
            format!("it did not install that one; run shoal update --version {OWN_VERSION} on it"),
            FOLLOW_GRACE,
        ),
        (_, true) => advice("run shoal hub update".into(), Duration::ZERO),
        (Updates::Homebrew, false) => advice(
            format!("run {} on it", shoal_core::update::HOMEBREW_UPGRADE),
            Duration::ZERO,
        ),
        (Updates::Manual, false) => advice(
            format!("run shoal update --version {OWN_VERSION} on it"),
            Duration::ZERO,
        ),
    }
}

/// Each member the hub refuses for another version, or any joiner: since
/// when, and when it last alerted the user to it; a recorded member or
/// none, so it stays as small as the members are many.
#[derive(Default)]
pub(super) struct RefusalAlerts(std::sync::Mutex<HashMap<Option<MemberId>, Refused>>);

struct Refused {
    since: Instant,
    alerted: Option<Instant>,
}

impl RefusalAlerts {
    /// Whether an alert about `member`, none for a joiner, is due once the
    /// hub has refused it for `grace`, which it then is not for
    /// [`ALERT_EVERY`].
    pub(super) fn due(&self, member: Option<MemberId>, grace: Duration) -> bool {
        let now = Instant::now();
        let mut refused = self.0.lock().unwrap();
        let refused = refused.entry(member).or_insert(Refused {
            since: now,
            alerted: None,
        });
        let due = now.duration_since(refused.since) >= grace
            && (refused.alerted).is_none_or(|alerted| now.duration_since(alerted) >= ALERT_EVERY);
        if due {
            refused.alerted = Some(now);
        }
        due
    }

    /// Forget the refusals of `member`, which the hub took.
    pub(super) fn taken(&self, member: &MemberId) {
        self.0.lock().unwrap().remove(&Some(member.clone()));
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

/// A hello whose member proved its key, with what sealing the link takes.
struct Proven {
    hello: Hello,
    exchange: Exchange,
    proof: Proof,
    transcript: Transcript,
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

/// The refusal of a link to a daemon that does not take the hub role.
fn not_hub() -> Refusal {
    refusal(
        RefusalReason::NotHub,
        "this daemon is not the hub; set [machine.hub] enabled there",
    )
}

/// Log that a link failed to open, with `error` as the hub shows what
/// another side sent.
fn log_failed(error: &anyhow::Error) {
    log!(
        "hub: a link failed to open: {}",
        shown(&format!("{error:#}"))
    );
}

fn refusal(reason: RefusalReason, message: impl Into<String>) -> Refusal {
    Refusal {
        reason,
        message: message.into(),
        version: None,
    }
}

/// Whether the member that sent `opening` in a request of `protocol` runs
/// the hub's own release.
fn same_release(protocol: u32, opening: &Value) -> bool {
    protocol == protocol::VERSION && claimed_version(opening) == OWN_VERSION
}

/// The Shoal version `opening` claims, held to a short run of the
/// characters a version takes.
fn claimed_version(opening: &Value) -> String {
    let claimed = opening["version"].as_str().unwrap_or_default();
    let version: String = (claimed.chars())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .collect();
    match version.is_empty() {
        true => "of another release".into(),
        false => shoal_protocol::validate::shorten(version, 32),
    }
}

#[cfg(test)]
mod tests;
