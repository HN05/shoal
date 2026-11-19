//! Opening a member's link: reaching the hub by a route, then proving this
//! machine's key and checking the hub's against the one it expects.
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    hub::{
        Fingerprint, Nonce, PublicKey, Route, Signature,
        link::{Frame, Hello, HubVersion, Intent, Opening, Refusal, Signer, Transcript, Updates},
    },
    protocol::{Method, Request, VERSION},
};

use super::connect::Transport;
use crate::{
    Hub,
    identity::{Identity, nonce, verify},
    pairing::code_proof,
    sealing::Exchange,
    shown,
    transport::Channel,
};

/// How long the hub has to answer each step of opening a link.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// A link to open: where, what for, the key pair this machine proves, the
/// pairing code a join proves, the key the hub must prove, and how this
/// machine comes to run the hub's version.
pub(super) struct Plan {
    pub route: Route,
    pub intent: Intent,
    pub key: Identity,
    pub code: Option<String>,
    pub expected: Expected,
    pub updates: Updates,
}

/// What opening a link came to.
pub(super) enum Opened {
    Welcomed(Welcomed),
    /// The hub refused the link: for sure when the refusal came `sealed`,
    /// and otherwise as anyone between the sides could have.
    Refused(LinkRefused),
}

/// The hub's refusal of a link, whether it came `sealed`, and the version
/// it says the hub runs, once the hub's signature of it checks.
pub(super) struct LinkRefused {
    pub refusal: Refusal,
    pub sealed: bool,
    pub hub_version: Option<String>,
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

impl Hub {
    /// Open the link `plan` names.
    pub(super) async fn open_link(&self, plan: Plan) -> Result<(Transport, Opened)> {
        let Plan {
            route,
            intent,
            key,
            code,
            expected,
            updates,
        } = plan;
        let late = || {
            anyhow!(
                "the hub at {route} did not answer within {}s",
                OPEN_TIMEOUT.as_secs()
            )
        };
        let connected = tokio::time::timeout(OPEN_TIMEOUT, Transport::connect(&route)).await;
        let mut transport = connected.map_err(|_| late())??;
        let opened = self.open_on(transport.channel(), intent, &key, code, expected, updates);
        match tokio::time::timeout(OPEN_TIMEOUT, opened).await {
            Ok(Ok(opened)) => Ok((transport, opened)),
            Ok(Err(error)) => Err(transport.failure(error).await),
            Err(_) => Err(late()),
        }
    }

    /// Open the link on `channel` for `intent`, proving `identity` and
    /// `code`, and stating that this machine `updates` so, and seal it once
    /// this side sent its proof.
    async fn open_on(
        &self,
        channel: &mut Channel,
        intent: Intent,
        identity: &Identity,
        code: Option<String>,
        expected: Expected,
        updates: Updates,
    ) -> Result<Opened> {
        let exchange = Exchange::new()?;
        let hello = Hello {
            version: env!("CARGO_PKG_VERSION").into(),
            nonce: nonce(),
            exchange: exchange.public.clone(),
            instance: self.hub_instance(),
            updates,
            intent,
        };
        channel.send(&opening(&hello)?).await?;
        let (key, transcript) = match channel.expect::<Frame>().await? {
            Frame::Refused(refusal) => {
                let hub_version = hub_version(&refusal, &hello.nonce, &expected);
                return Ok(Opened::Refused(LinkRefused {
                    refusal,
                    sealed: false,
                    hub_version,
                }));
            }
            Frame::Challenge {
                key,
                nonce,
                exchange: theirs,
                signature,
            } => {
                let transcript = Transcript {
                    hello,
                    hub_nonce: nonce,
                    hub_exchange: theirs,
                };
                check_hub(&key, &transcript, &signature, &expected)?;
                (key, transcript)
            }
            frame => bail!(
                "expected the hub's challenge, not {}",
                shown(&format!("{frame:?}"))
            ),
        };
        let signed = transcript.signed(Signer::Member);
        let code = code.map(|code| code_proof(&code, &signed));
        let signature = identity.sign(&signed);
        channel.send(&Frame::Proof { signature, code }).await?;
        channel
            .seal(exchange.keys(&transcript, Signer::Member)?)
            .await?;
        Ok(match channel.expect::<Frame>().await? {
            Frame::Welcome { member, name, .. } => Opened::Welcomed(Welcomed {
                member,
                name,
                hub_key: key,
            }),
            Frame::Refused(refusal) => Opened::Refused(LinkRefused {
                hub_version: hub_version(&refusal, &transcript.hello.nonce, &expected),
                refusal,
                sealed: channel.sealed_last(),
            }),
            frame => bail!(
                "expected the hub's welcome, not {}",
                shown(&format!("{frame:?}"))
            ),
        })
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

/// Refuse a hub whose `signature` of `transcript` does not check with
/// `key`, or whose key is not the `expected` one.
fn check_hub(
    key: &PublicKey,
    transcript: &Transcript,
    signature: &Signature,
    expected: &Expected,
) -> Result<()> {
    if !verify(key, &transcript.signed(Signer::Hub), signature) {
        bail!("the hub's signature does not check");
    }
    if !expected.admits(key) {
        bail!(
            "the hub's key is not the one this machine paired with, so something between them poses as the hub"
        );
    }
    Ok(())
}

/// The version `refusal` says the hub runs, once the hub's signature of
/// it over `nonce`, this machine's, checks with a key `expected` admits.
fn hub_version(refusal: &Refusal, nonce: &Nonce, expected: &Expected) -> Option<String> {
    let signed = refusal.version.as_ref()?;
    let bytes = HubVersion::signed(&signed.version, nonce);
    let checks = expected.admits(&signed.key) && verify(&signed.key, &bytes, &signed.signature);
    checks.then(|| signed.version.clone())
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
