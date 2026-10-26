//! A local HTTP proxy between an AI agent and its API upstream, such as
//! CLIProxyAPI or a provider's own API. It forwards each request with the
//! agent's own credentials. It depends on no other Shoal code: Shoal runs one
//! per agent and decides what its reports mean.
mod forward;
mod upstream;

use std::{net::SocketAddr, sync::Arc};

use tokio::net::TcpListener;

pub use upstream::Upstream;

/// A proxy listening on a loopback port.
pub struct Proxy {
    listener: TcpListener,
    forwarder: Arc<forward::Forwarder>,
}

impl Proxy {
    /// Listen on a free loopback port for requests to forward to `upstream`.
    pub async fn bind(upstream: Upstream) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        Ok(Self {
            listener,
            forwarder: Arc::new(forward::Forwarder::new(upstream)),
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
