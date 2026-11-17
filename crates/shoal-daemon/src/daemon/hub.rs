//! The hub role and membership of a hub. A daemon with `[machine.hub]
//! enabled` is the hub: it listens for its members' links and keeps their
//! credentials, while it stays the machine of its own spaces. Any other
//! daemon that joined a hub keeps one link to it, reconnecting with backoff,
//! over a WebSocket to the URL it paired at or over `[machine.hub] connect`.
//! The hub never dials a member; what it asks of one goes down that link.
use std::sync::Arc;

use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{HubRole, Overview};

use super::workspace::Manager;

mod accept;
pub(crate) mod answers;
mod identity;
mod links;
mod listener;
mod member;
mod members;
mod membership;
mod pairing;
mod session;
mod transport;

/// What the hub role and membership keep while the daemon runs.
#[derive(Default)]
pub(crate) struct Hub {
    identity: tokio::sync::OnceCell<Arc<identity::Identity>>,
    /// As the hub: the pairing codes it issued.
    codes: pairing::Codes,
    /// As the hub: its members' open links.
    links: links::Links,
    /// As the hub: where its listener is bound, or why it is not.
    listener: listener::Bound,
    /// As a member: its link to the hub.
    member: member::Link,
}

/// Serve the hub's listener while this daemon is the hub, and keep a link
/// to the hub while it is a member of one, until the daemon stops.
pub(crate) async fn run(manager: Arc<Manager>) {
    tokio::join!(listener::run(manager.clone()), member::run(manager));
}

/// Serve the link a member opened on the daemon's socket with `hello`, in a
/// request of `protocol`, until it ends.
pub(crate) async fn accept_socket_link(
    manager: Arc<Manager>,
    stream: tokio::net::UnixStream,
    protocol: u32,
    hello: serde_json::Value,
) {
    let (reader, writer) = stream.into_split();
    let channel = transport::Channel::lines(tokio::io::BufReader::new(reader), writer);
    (manager.accept_member(channel, protocol, hello, accept::Over::Socket)).await;
}

impl Manager {
    /// `[machine.hub]`, none in a daemon that leaves the user's config to
    /// the installed Shoal, which takes the hub role there.
    pub(crate) fn hub_settings(&self) -> Option<shoal_core::config::machine::Hub> {
        (self.acts_on_global_config()).then(|| self.config().machine.hub.clone())
    }

    /// Whether this daemon takes the hub role.
    pub(crate) fn is_hub(&self) -> bool {
        self.hub_settings().is_some_and(|hub| hub.enabled)
    }

    /// The hub role this daemon takes, for `shoal status`.
    pub(crate) async fn hub_role(&self) -> Result<Option<HubRole>> {
        if self.is_hub() {
            return Ok(Some(HubRole::Hub {
                listener: self.hub_listener(),
            }));
        }
        self.membership_role().await
    }

    /// The hub's listener and members, from the hub itself or through this
    /// machine's link to it.
    pub(crate) async fn hub_overview(&self) -> Result<Overview> {
        if self.is_hub() {
            return self.local_hub_overview().await;
        }
        self.through_hub(shoal_protocol::hub::link::Call::Members)
            .await
            .and_then(answers::overview)
    }

    /// The ID of the hub's own machine, recording it the first time.
    async fn own_member(&self) -> Result<MemberId> {
        let own = self.hub_identity().await?.public.clone();
        let name = crate::process::machine::hostname();
        self.store
            .write(move |tx| {
                members::record_own(tx, &own, &name)?;
                members::list(tx, &own)?
                    .into_iter()
                    .find(|member| member.hub)
                    .map(|member| member.id)
                    .ok_or_else(|| anyhow::anyhow!("the hub's own machine is not recorded"))
            })
            .await
    }
}
