//! Opening a member's link: reaching the hub by a route, then proving this
//! machine's key and checking the hub's against the one it expects.
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    hub::{
        Fingerprint, MemberKind, PublicKey, Route,
        link::{Frame, Hello, Intent, Joining, Opening, Refusal, Signer, signed},
    },
    protocol::{Method, Request, VERSION},
};

use super::connect::Transport;
use crate::daemon::{
    hub::{
        identity::{nonce, verify},
        transport::Channel,
    },
    workspace::Manager,
};

/// How long the hub has to answer each step of opening a link.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// A link to open: where, what for, and the key the hub must prove.
pub(super) struct Plan {
    pub route: Route,
    pub intent: Intent,
    pub expected: Expected,
}

/// What opening a link came to.
pub(super) enum Opened {
    Welcomed(Welcomed),
    Refused(Refusal),
}

/// The hub took this machine as `member`, named `name`; it holds `hub_key`.
pub(super) struct Welcomed {
    pub member: MemberId,
    pub name: String,
    pub hub_key: PublicKey,
}

/// The key the hub must prove.
pub(super) enum Expected {
    /// The one this machine paired with.
    Key(PublicKey),
    /// The one a pairing code names.
    Fingerprint(Fingerprint),
    /// Whichever answers through a command, which runs as the user there.
    Any,
}

impl Expected {
    fn admits(&self, key: &PublicKey) -> bool {
        match self {
            Self::Key(expected) => expected == key,
            Self::Fingerprint(expected) => expected == &key.fingerprint(),
            Self::Any => true,
        }
    }
}

impl Manager {
    /// Open the link `plan` names.
    pub(super) async fn open_link(&self, plan: Plan) -> Result<(Transport, Opened)> {
        let Plan {
            route,
            intent,
            expected,
        } = plan;
        let mut transport = Transport::connect(&route).await?;
        let opened = self.open_on(transport.channel(), intent, expected);
        match tokio::time::timeout(OPEN_TIMEOUT, opened).await {
            Ok(Ok(opened)) => Ok((transport, opened)),
            Ok(Err(error)) => Err(transport.failure(error).await),
            Err(_) => bail!(
                "the hub at {route} did not answer within {}s",
                OPEN_TIMEOUT.as_secs()
            ),
        }
    }

    /// Open the link on `channel` for `intent`.
    async fn open_on(
        &self,
        channel: &mut Channel,
        intent: Intent,
        expected: Expected,
    ) -> Result<Opened> {
        let identity = self.hub_identity().await?;
        let ours = nonce();
        let hello = Hello {
            version: env!("CARGO_PKG_VERSION").into(),
            nonce: ours.clone(),
            instance: self.hub_instance(),
            intent,
        };
        channel.send(&opening(&hello)?).await?;
        let (key, theirs) = match channel.expect::<Frame>().await? {
            Frame::Refused(refusal) => return Ok(Opened::Refused(refusal)),
            Frame::Challenge {
                key,
                nonce,
                signature,
            } => {
                if !verify(&key, &signed(Signer::Hub, &ours, &nonce), &signature) {
                    bail!("the hub's signature does not check");
                }
                if !expected.admits(&key) {
                    bail!(
                        "the hub's key is not the one this machine paired with, so something between them poses as the hub"
                    );
                }
                (key, nonce)
            }
            frame => bail!("expected the hub's challenge, not {frame:?}"),
        };
        let signature = identity.sign(&signed(Signer::Member, &theirs, &ours));
        channel.send(&Frame::Proof { signature }).await?;
        Ok(match channel.expect::<Frame>().await? {
            Frame::Welcome { member, name, .. } => Opened::Welcomed(Welcomed {
                member,
                name,
                hub_key: key,
            }),
            Frame::Refused(refusal) => Opened::Refused(refusal),
            frame => bail!("expected the hub's welcome, not {frame:?}"),
        })
    }

    /// The intent of joining as a machine named `name`, or after its host,
    /// with `code`.
    pub(super) async fn join_intent(
        &self,
        name: Option<String>,
        code: Option<String>,
    ) -> Result<Intent> {
        Ok(Intent::Join(Joining {
            kind: MemberKind::Machine,
            name: name.unwrap_or_else(crate::process::machine::hostname),
            key: self.hub_identity().await?.public.clone(),
            code,
        }))
    }

    /// What tells this installation from a copy of its state directory
    /// elsewhere: its machine and its state directory.
    fn hub_instance(&self) -> String {
        use sha2::Digest as _;
        let machine =
            crate::process::machine::id().unwrap_or_else(crate::process::machine::hostname);
        let digest = sha2::Sha256::new()
            .chain_update(machine)
            .chain_update([0])
            .chain_update(self.paths.state.as_os_str().as_encoded_bytes())
            .finalize();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// The request that opens a link with `hello`.
fn opening(hello: &Hello) -> Result<Request> {
    let hello = serde_json::to_value(hello).context("write the hub link hello")?;
    Ok(Request {
        protocol: VERSION,
        id: 1,
        method: Method::HubLink(Opening::HubLink(hello)),
        scope: None,
    })
}
