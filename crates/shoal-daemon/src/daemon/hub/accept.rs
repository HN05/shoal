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
        Member, MemberKind, PublicKey, Signature,
        link::{Frame, Hello, Intent, Joining, Refusal, RefusalReason, Signer, signed},
    },
    protocol,
};

use super::{
    identity::{nonce, verify},
    links::Registration,
    members,
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

    /// The member that opened `channel`, or why the hub refuses it.
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
        let Some(proof) = self.challenge(channel, &hello).await? else {
            return Ok(Err(refusal(
                RefusalReason::Proof,
                "no proof of the member's key",
            )));
        };
        let member = match &hello.intent {
            Intent::Join(joining) => self.joining(joining, &proof, over).await?,
            Intent::Connect { member } => self.paired(member, &proof).await?,
        };
        let member = match member {
            Ok(member) => member,
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
        channel.send(&welcome).await?;
        Ok(Ok(Admitted {
            member,
            version: hello.version,
            registration,
        }))
    }

    /// Prove the hub's key to the member that sent `hello`, and read its
    /// proof of its own, none when it sends none in time.
    async fn challenge(&self, channel: &mut Channel, hello: &Hello) -> Result<Option<Proof>> {
        let identity = self.hub_identity().await?;
        let ours = nonce();
        let challenge = Frame::Challenge {
            key: identity.public.clone(),
            signature: identity.sign(&signed(Signer::Hub, &hello.nonce, &ours)),
            nonce: ours.clone(),
        };
        channel.send(&challenge).await?;
        let frame = tokio::time::timeout(PROOF_TIMEOUT, channel.expect::<Frame>()).await;
        match frame {
            Ok(Ok(Frame::Proof { signature })) => Ok(Some(Proof {
                signed: signed(Signer::Member, &ours, &hello.nonce),
                signature,
            })),
            Ok(Ok(frame)) => bail!("expected the member's proof, not {frame:?}"),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    /// The new member `joining` asks for, which `proof` must prove, recorded
    /// once a pairing code admits it, or a link over the daemon's socket,
    /// or why the hub refuses it.
    async fn joining(
        &self,
        joining: &Joining,
        proof: &Proof,
        over: Over,
    ) -> Result<Result<Member, Refusal>> {
        let Joining {
            kind,
            name,
            key,
            code,
        } = joining;
        if !proof.proves(key) {
            return Ok(Err(refusal(
                RefusalReason::Proof,
                "the joiner's signature does not check",
            )));
        }
        let admitted =
            over == Over::Socket || (code.as_deref()).is_some_and(|code| self.hub.codes.take(code));
        if !admitted {
            return Ok(Err(refusal(
                RefusalReason::Code,
                "the pairing code is unknown, used or expired; run shoal hub pair for a new one",
            )));
        }
        self.join_member(*kind, name, key).await.map(Ok)
    }

    /// Record a new member of `kind` named `name`, which proved `key`.
    async fn join_member(&self, kind: MemberKind, name: &str, key: &PublicKey) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let (name, key) = (name.to_owned(), key.clone());
        self.store
            .write(move |tx| {
                let id = members::join(tx, kind, &name, &key)?;
                let recorded = members::find(tx, &id, &own)?;
                Ok(recorded.context("a member just recorded")?.member)
            })
            .await
    }

    /// The paired member `id`, which `proof` must prove, or why the hub
    /// refuses it.
    async fn paired(&self, id: &MemberId, proof: &Proof) -> Result<Result<Member, Refusal>> {
        let own = self.hub_identity().await?.public.clone();
        let id = id.clone();
        let recorded = self
            .store
            .read(move |tx| members::find(tx, &id, &own))
            .await?;
        Ok(match recorded {
            None => Err(refusal(
                RefusalReason::Proof,
                "the hub has no such member; run shoal hub join to join it again",
            )),
            Some(recorded) if !proof.proves(&recorded.key) => Err(refusal(
                RefusalReason::Proof,
                "the member's signature does not check",
            )),
            Some(recorded) if recorded.revoked => Err(refusal(
                RefusalReason::Revoked,
                format!(
                    "the hub revoked the credential of {} {}; run shoal hub join to join it again",
                    recorded.member.kind, recorded.member.name
                ),
            )),
            Some(recorded) => Ok(recorded.member),
        })
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
            async move { session::reply(manager.answer_member(&member, call).await) }
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

/// A member's signature and what it must sign.
struct Proof {
    signed: Vec<u8>,
    signature: Signature,
}

impl Proof {
    fn proves(&self, key: &PublicKey) -> bool {
        verify(key, &self.signed, &self.signature)
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
