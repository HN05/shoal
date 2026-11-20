//! The hub role and membership of a hub. A daemon with `[machine.hub]
//! enabled` is the hub: it listens for its members' links and keeps their
//! credentials, while it stays the machine of its own spaces. Any other
//! daemon that joined a hub keeps one link to it, reconnecting with backoff,
//! over a WebSocket to the URL it paired at or over `[machine.hub] connect`.
//! The hub never dials a member; what it asks of one goes down that link.
//! [`Hub`] holds what the hub keeps in memory and offers its operations;
//! what it needs of the rest of the daemon is the interface in [`daemon`].
use std::sync::Arc;

use shoal_core::{agents, config, process, refusal, repository};
use shoal_daemon_base::{
    config_state::ConfigState,
    gates::Gates,
    log,
    notifications::{About, Notifications},
};
use shoal_protocol::notifications::NotificationKind;
use shoal_protocol::paths::Paths;
use shoal_protocol::time;
use shoal_store::Store;

pub use forwarding::{Elsewhere, Unanswered};
pub use machines::Named;
pub use streams::{Incoming, Outgoing};
pub use telling::Event;

mod accept;
pub mod answers;
mod app_servers;

mod availability;
mod battery;
pub mod daemon;
pub mod directory;
mod escalation;
mod forwarding;
pub mod identity;
mod installer;
mod known_tree;
pub mod links;
pub mod listener;
mod machines;
mod member;
pub mod members;
pub mod membership;
pub mod numbering;
mod owners;
mod pairing;
pub mod pieces;
pub mod presence;
mod queue;

pub mod removal;
mod reports;
mod role;
pub mod routing;
pub mod sealing;
pub mod serving;
pub mod session;
#[cfg(test)]
mod stand_in;
pub mod streams;
mod telling;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transport;
mod tree;
mod updating;
mod view;

use daemon::Daemon;
pub use removal::BelowElsewhere;

/// The hub role and membership of this machine, with the daemon's services
/// they use.
pub struct Hub {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    paths: Paths,
    records: Arc<dyn daemon::Records>,
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

impl Hub {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        paths: Paths,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            paths,
            records,
            identity: Default::default(),
            codes: Default::default(),
            links: Default::default(),
            admissions: Default::default(),
            refusal_alerts: Default::default(),
            listener: Default::default(),
            member: Default::default(),
            vouches: Default::default(),
            outbox: Default::default(),
            deliveries: Default::default(),
            known_tree: Default::default(),
            parent_permissions: Default::default(),
        }
    }
}

impl Hub {
    /// Tell the user `message`, of `kind`, about what `about` names, or
    /// its coordinator.
    async fn notify(
        &self,
        daemon: &dyn Daemon,
        about: About,
        kind: NotificationKind,
        message: String,
    ) {
        (self.notifications)
            .notify(daemon, about, kind, message, &[])
            .await;
    }
}

/// Serve the hub's listener and keep its own machine's spaces in its
/// directory while this daemon is the hub, and keep a link to the hub while
/// it is a member of one, until the daemon stops; whether the daemon
/// `restarts_on_update` decides what following the hub's version tells the
/// user.
pub async fn run(hub: Arc<Hub>, daemon: Arc<dyn Daemon>, restarts_on_update: bool) {
    hub.member.restarts_on_update(restarts_on_update);
    tokio::join!(
        listener::run(hub.clone(), daemon.clone()),
        hub.report_own_spaces(&*daemon),
        hub.expire_held(&*daemon),
        hub.escalate_waiting(&*daemon),
        hub.keep_marking_presence(&*daemon),
        member::run(hub.clone(), daemon.clone()),
    );
}

/// Serve the link a member opened on the daemon's socket with `hello`, in a
/// request of `protocol`, until it ends.
pub async fn accept_socket_link(
    hub: Arc<Hub>,
    daemon: Arc<dyn Daemon>,
    stream: tokio::net::UnixStream,
    protocol: u32,
    hello: serde_json::Value,
) {
    let (reader, writer) = stream.into_split();
    let channel = transport::Channel::lines(tokio::io::BufReader::new(reader), writer);
    (hub.accept_member(&daemon, channel, protocol, hello, accept::Over::Socket)).await;
}

/// The most of what the other side of a link sent that the daemon keeps or
/// shows.
pub const SHOWN_TEXT: usize = 300;

/// `text`, which may quote what the other side of a link sent before it was
/// authenticated, as the daemon keeps and shows it: one line of printable
/// text, at most [`SHOWN_TEXT`] bytes.
pub fn shown(text: &str) -> String {
    let printable: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    shoal_protocol::validate::shorten(printable, SHOWN_TEXT)
}

#[cfg(test)]
mod tests;
