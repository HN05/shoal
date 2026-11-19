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

use super::manager::Manager;

pub(crate) use forwarding::{Elsewhere, Unanswered};
pub(crate) use streams::{Incoming, Outgoing};
pub(crate) use telling::Event;

mod accept;
pub(crate) mod answers;
mod app_servers;
mod apps;
mod directory;
mod escalation;
mod forwarding;
mod identity;
mod known_tree;
mod links;
mod listener;
mod member;
mod members;
mod membership;
pub(crate) mod numbering;
mod owners;
mod pairing;
mod pieces;
mod queue;
mod removal;
mod reports;
mod routing;
mod sealing;
mod serving;
mod session;
mod streams;
mod telling;
mod transport;
mod tree;
mod view;

/// What the hub role and membership keep while the daemon runs.
#[derive(Default)]
pub(crate) struct Hub {
    identity: tokio::sync::OnceCell<Arc<identity::Identity>>,
    /// As the hub: the pairing codes it issued.
    codes: pairing::Codes,
    /// As the hub: its members' open links.
    links: links::Links,
    /// As the hub: held while a member is admitted and its link registered,
    /// and while a member is revoked and its link ended.
    admissions: tokio::sync::Mutex<()>,
    /// As the hub: when it last alerted the user to each refused member.
    refusal_alerts: accept::RefusalAlerts,
    /// As the hub: where its listener is bound, or why it is not.
    listener: listener::Bound,
    /// As the hub: the nonces of the requests it forwarded lately.
    nonces: app_servers::Nonces,
    /// As a member: its link to the hub.
    member: member::Link,
    /// The callers other machines vouch for, while this machine answers
    /// their forwarded requests.
    vouches: serving::Vouches,
    /// As a member: wakes the sending of what its outbox keeps.
    outbox: tokio::sync::Notify,
    /// As the hub: who delivers what it holds for each machine now.
    deliveries: queue::Deliveries,
    /// As a member: the tree across machines it last learned, as its store
    /// keeps it.
    known_tree: std::sync::Mutex<Option<known_tree::Known>>,
    /// The effective permissions the hub last listed of each space on
    /// another machine above a space here, which stand while it cannot be
    /// asked.
    parent_permissions:
        std::sync::Mutex<std::collections::HashMap<shoal_ids::space_ids::SpaceId, tree::Reported>>,
}

/// Serve the hub's listener and keep its own machine's spaces in its
/// directory while this daemon is the hub, and keep a link to the hub while
/// it is a member of one, until the daemon stops.
pub(crate) async fn run(manager: Arc<Manager>) {
    tokio::join!(
        listener::run(manager.clone()),
        manager.report_own_spaces(),
        manager.expire_held(),
        manager.escalate_waiting(),
        member::run(manager.clone()),
    );
}

/// The most of what the other side of a link sent that the daemon keeps or
/// shows.
const SHOWN_TEXT: usize = 300;

/// `text`, which may quote what the other side of a link sent before it was
/// authenticated, as the daemon keeps and shows it: one line of printable
/// text, at most [`SHOWN_TEXT`] bytes.
fn shown(text: &str) -> String {
    let printable: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    shoal_protocol::validate::shorten(printable, SHOWN_TEXT)
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

    /// The hub's own machine, recording it the first time.
    async fn own_machine(&self) -> Result<shoal_protocol::hub::Member> {
        let own = self.own_member().await?;
        let members = self.local_hub_overview().await?.members;
        (members.into_iter())
            .find(|member| member.id == own)
            .ok_or_else(|| anyhow::anyhow!("the hub's own machine is not recorded"))
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

#[cfg(test)]
mod tests;
