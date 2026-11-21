//! The hub: pairing codes, joining and leaving it, its machines and devices,
//! and the relay that carries a member's link to this machine's daemon over
//! a command's standard input and output.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::{HubUpdate, Left, Member, Membership, Overview, pairing::Pairing},
    paths::Paths,
    protocol::{HubMethod, JoinVia},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

use crate::connection::{Daemon, request};

/// A new pairing code from the hub.
pub async fn pair(daemon: &impl Daemon) -> Result<Pairing> {
    request(daemon, HubMethod::PairHub).await
}

/// Join the hub `via` reaches, as a machine named `name`, or after its host.
pub async fn join(daemon: &impl Daemon, via: JoinVia, name: Option<String>) -> Result<Membership> {
    request(daemon, HubMethod::JoinHub { via, name }).await
}

/// Leave the hub, revoking this machine's credential there.
pub async fn leave(daemon: &impl Daemon) -> Result<Left> {
    request(daemon, HubMethod::LeaveHub).await
}

/// Install the latest release on the hub.
pub async fn update(daemon: &impl Daemon) -> Result<HubUpdate> {
    request(daemon, HubMethod::UpdateHub).await
}

/// The hub's listener and members.
pub async fn members(daemon: &impl Daemon) -> Result<Overview> {
    request(daemon, HubMethod::HubMembers).await
}

/// Revoke the credential of the machine or device `member` names by ID or
/// name.
pub async fn revoke(daemon: &impl Daemon, member: String) -> Result<Member> {
    request(daemon, HubMethod::RevokeHubMember { member }).await
}

/// Carry `input` to the socket of the daemon `paths` names and its replies
/// to `output`, byte for byte, until the daemon closes the connection and
/// all it sent before has gone to `output`; the end of `input` closes the
/// daemon's side for reading. It never starts a daemon: one that does not
/// run fails it.
pub async fn relay(
    paths: &Paths,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<()> {
    let mut socket = tokio::net::UnixStream::connect(&paths.socket)
        .await
        .with_context(|| format!("no Shoal daemon runs for {}", paths.state.display()))?;
    let (mut from_daemon, to_daemon) = socket.split();
    let upstream = async move {
        send_to_daemon(input, to_daemon).await;
        std::future::pending().await
    };
    let downstream = async {
        tokio::io::copy(&mut from_daemon, &mut output).await?;
        output.flush().await
    };
    let relayed: std::io::Result<()> = tokio::select! {
        never = upstream => never,
        relayed = downstream => relayed,
    };
    relayed.context("relay to the daemon")
}

/// Carry `input` to the daemon until either side ends, then close the
/// daemon's side for reading and let `input` go. A daemon that closed the
/// connection fails the copy, which ends nothing: what it wrote before,
/// such as its reply to a member that leaves, still goes to the output.
async fn send_to_daemon(mut input: impl AsyncRead + Unpin, mut to_daemon: impl AsyncWrite + Unpin) {
    let _ = tokio::io::copy(&mut input, &mut to_daemon).await;
    let _ = to_daemon.shutdown().await;
}

#[cfg(test)]
mod tests;
