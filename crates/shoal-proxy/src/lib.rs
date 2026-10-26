//! A local HTTP proxy between an AI agent and its API upstream, such as
//! CLIProxyAPI or a provider's own API. It forwards each request with the
//! agent's own credentials. It depends on no other Shoal code: Shoal runs one
//! per agent and decides what its reports mean.
mod clock;
mod forward;
mod retry;
mod stream;
mod upstream;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{net::TcpListener, sync::mpsc::UnboundedSender};

pub use retry::Failure;
pub use upstream::Upstream;

/// What the proxy tells its owner about the requests it retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Attempt `attempt` failed, and the proxy tries again after `wait`.
    Retrying {
        failure: Failure,
        attempt: u32,
        wait: Duration,
    },
    /// The proxy gave up on a failure it retries, so the agent gets it;
    /// `in_stream` when it arrives inside a response stream.
    Failed { failure: Failure, in_stream: bool },
    /// A request the proxy retried succeeded.
    Recovered,
}

/// A proxy listening on a loopback port.
pub struct Proxy {
    listener: TcpListener,
    forwarder: Arc<forward::Forwarder>,
}

impl Proxy {
    /// Listen on a free loopback port for requests to forward to `upstream`,
    /// telling `events` about retries.
    pub async fn bind(upstream: Upstream, events: UnboundedSender<Event>) -> std::io::Result<Self> {
        let clock = Arc::new(clock::Tokio::new());
        Self::bind_with(upstream, events, retry::POLICY, clock).await
    }

    async fn bind_with(
        upstream: Upstream,
        events: UnboundedSender<Event>,
        policy: retry::Policy,
        clock: Arc<dyn clock::Clock>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let forwarder = forward::Forwarder::new(upstream, events, policy, clock);
        Ok(Self {
            listener,
            forwarder: Arc::new(forwarder),
        })
    }

    /// The address agents send their requests to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve each connection until the listener fails.
    pub async fn serve(self) -> std::io::Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            tokio::spawn(self.forwarder.clone().serve_connection(stream));
        }
    }
}

#[cfg(test)]
mod tests;
