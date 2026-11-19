//! Streams to running demos: after its answer, a client's connection
//! carries the bytes of one TCP connection the daemon opens to a `server`
//! or `web` demo's port on loopback, both ways and unread, so WebSockets and
//! the like work as they do on the demo's own machine.
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    demos::{Kind, Running},
    protocol::{self, Body, Response},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use tokio::net::{TcpStream, UnixStream};

use crate::{
    daemon::{answer::OperationGuard, manager::Manager},
    refusal::{Reason, Refusal},
};

/// How often a demo stream checks that the scope token it came with still
/// acts for a caller.
const SCOPE_CHECK: Duration = Duration::from_secs(1);

/// A running demo a stream opens to, with the connection to it.
pub(crate) struct OpenDemo {
    demo: Running,
    connection: TcpStream,
}

impl Manager {
    /// Connect to the running demo `name` of the workspace `selector`
    /// names; refused for a demo of a kind Shoal does not stream.
    pub(crate) async fn open_demo(&self, selector: &SpaceSelector, name: &str) -> Result<OpenDemo> {
        crate::demos::validate_name(name)?;
        let running = self.demos(Some(selector)).await?;
        let Some(demo) = running.into_iter().find(|demo| demo.launch.name == name) else {
            let message = format!("space {selector} has no running demo {name}");
            return Err(Refusal::new(Reason::NotFound, message).into());
        };
        let port = match demo.kind {
            Kind::Server | Kind::Web => demo.launch.port(),
            Kind::Terminal | Kind::Simulator => None,
        };
        let Some(port) = port else {
            let message = format!(
                "demo {name} is a {} demo, which Shoal does not stream; open it on its own machine",
                demo.kind
            );
            return Err(Refusal::new(Reason::NotReady, message).into());
        };
        let connection = connect(port)
            .await
            .with_context(|| format!("demo {name} takes no connection on port {port}"))
            .map_err(|error| Refusal::wrap(Reason::NotReady, error))?;
        Ok(OpenDemo { demo, connection })
    }

    /// Answer the request `id` with `open`'s demo, then relay the bytes
    /// of `stream` and of the demo's connection both ways, until both
    /// sides end or either fails, or the scope token the request came
    /// with, `scope`, stops acting for a caller.
    pub(crate) async fn relay_demo(
        self: Arc<Self>,
        mut stream: UnixStream,
        id: u64,
        (open, scope): (OpenDemo, Option<ScopeToken>),
        startup: OperationGuard,
    ) -> Result<()> {
        let OpenDemo {
            demo,
            mut connection,
        } = open;
        let answer = Response::new(id, Body::DemoStream(Box::new(demo)));
        protocol::write(&mut stream, &answer).await?;
        drop(startup);
        tokio::select! {
            // A connection either side resets ends as one that closes.
            _ = tokio::io::copy_bidirectional(&mut stream, &mut connection) => {}
            () = self.scope_revoked(scope.as_ref()) => {}
        }
        Ok(())
    }

    /// Resolves once `scope` no longer acts for a caller; never for the
    /// user's.
    async fn scope_revoked(&self, scope: Option<&ScopeToken>) {
        let mut checks = tokio::time::interval(SCOPE_CHECK);
        loop {
            checks.tick().await;
            if let Ok(false) = self.still_scoped(scope).await {
                return;
            }
        }
    }
}

/// A connection to `port` on loopback: IPv4's, else IPv6's.
async fn connect(port: u16) -> Result<TcpStream> {
    match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(connection) => Ok(connection),
        Err(v4) => TcpStream::connect(("::1", port))
            .await
            .map_err(|_| v4.into()),
    }
}

#[cfg(test)]
mod tests;
