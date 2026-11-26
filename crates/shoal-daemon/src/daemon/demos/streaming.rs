//! Streams to running demos. After its answer, a client's connection
//! carries the bytes of one loopback TCP connection to the demo, both ways
//! and unread, or for a terminal demo a terminal the daemon makes for the
//! viewer.
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use shoal_demos::connect;
use shoal_protocol::{
    attaching::Terminal,
    demos::{Kind, Running},
    protocol::{self, Body, Response},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use tokio::net::{TcpStream, UnixStream};

use crate::{
    daemon::{
        answer::OperationGuard,
        manager::Manager,
        scope::Caller,
        viewer_terminals::{Reports, ViewerTerminal},
    },
    refusal::{Reason, Refusal},
    workers::{Worker, internal_command},
};

/// How often a demo stream checks that the scope token it came with still
/// acts for a caller.
const SCOPE_CHECK: Duration = Duration::from_secs(1);

/// A running demo a stream opens to, with what shows it.
pub(crate) struct OpenDemo {
    demo: Running,
    shown: Shown,
}

/// What a stream to a demo carries.
enum Shown {
    /// A connection to its port.
    Port(TcpStream),
    /// A terminal that shows its place.
    Terminal(Box<ViewerTerminal>),
}

impl Manager {
    /// Connect to the running demo `name` of the workspace `selector`
    /// names, a terminal demo on a terminal made like `terminal`; refused
    /// for a demo that takes no connection, such as a simulator demo whose
    /// viewer is off.
    pub(crate) async fn open_demo(
        &self,
        selector: &SpaceSelector,
        name: &str,
        terminal: &Terminal,
        caller: Option<&Caller>,
    ) -> Result<OpenDemo> {
        crate::demos::validate_name(name)?;
        let running = self.demos.list(self, Some(selector)).await?;
        let Some(mut demo) = running.into_iter().find(|demo| demo.launch.name == name) else {
            let message = format!("space {selector} has no running demo {name}");
            return Err(Refusal::new(Reason::NotFound, message).into());
        };
        let shown = match demo.kind {
            Kind::Terminal => {
                let terminal = self.show_terminal_demo(&demo, terminal).await?;
                Shown::Terminal(Box::new(terminal))
            }
            Kind::Server | Kind::Web | Kind::Simulator => {
                Shown::Port(self.connect_demo(&mut demo, caller).await?)
            }
        };
        Ok(OpenDemo { demo, shown })
    }

    /// Connect to the port `demo` serves on, a simulator demo's viewer's,
    /// which this starts and records in `demo`; refused for a demo that
    /// serves on none, such as a terminal demo or one whose `PORT` is gone.
    async fn connect_demo(&self, demo: &mut Running, caller: Option<&Caller>) -> Result<TcpStream> {
        let name = &demo.launch.name;
        let port = match demo.kind {
            Kind::Server | Kind::Web => demo.launch.port(),
            Kind::Simulator => {
                let port = self.demos.open_viewer(self, demo, caller).await?;
                demo.viewer = demo.launch.viewer(Some(port));
                Some(port)
            }
            Kind::Terminal => None,
        };
        let Some(port) = port else {
            let message = format!("demo {name} serves on no port");
            return Err(Refusal::new(Reason::NotReady, message).into());
        };
        connect(port)
            .await
            .with_context(|| format!("demo {name} takes no connection on port {port}"))
            .map_err(|error| Refusal::wrap(Reason::NotReady, error))
    }

    /// Start showing the terminal demo `demo`'s place on a terminal made
    /// like `terminal`, as its host shows a place to another machine.
    async fn show_terminal_demo(
        &self,
        demo: &Running,
        terminal: &Terminal,
    ) -> Result<ViewerTerminal> {
        let place = &demo.launch.origin;
        let machine = self.hub.machine_name(self).await;
        let place_json = serde_json::to_string(place)?;
        let worker = Worker::ShowDemo {
            name: &demo.launch.name,
            place: &place_json,
            machine: &machine,
            app: terminal.app(place.host),
        };
        let argv = internal_command(&self.paths, false, worker)?;
        ViewerTerminal::start(&argv, terminal, &self.paths.home, Reports::No)
    }

    /// Answer the request `id` with `open`'s demo, then relay what shows
    /// the demo and what `stream` brings both ways, until both sides end or
    /// either fails, or the scope token the request came with, `scope`,
    /// stops acting for a caller.
    pub(crate) async fn relay_demo(
        self: Arc<Self>,
        stream: UnixStream,
        id: u64,
        (open, scope): (OpenDemo, Option<ScopeToken>),
        startup: OperationGuard,
    ) -> Result<()> {
        let OpenDemo { demo, shown } = open;
        let answer = (id, Body::DemoStream(Box::new(demo)));
        let revoked = self.scope_revoked(scope.as_ref());
        match shown {
            Shown::Port(connection) => {
                relay_port(stream, answer, connection, revoked, startup).await
            }
            Shown::Terminal(terminal) => {
                let manager = self.clone();
                (manager.relay_viewer_terminal(stream, answer, *terminal, revoked, startup)).await
            }
        }
    }

    /// Resolves once `scope` no longer acts for a caller; never for the
    /// user's.
    async fn scope_revoked(&self, scope: Option<&ScopeToken>) {
        let mut checks = tokio::time::interval(SCOPE_CHECK);
        loop {
            checks.tick().await;
            if let Ok(false) = self.coordination.still_scoped(self, scope).await {
                return;
            }
        }
    }
}

/// Answer the request with `answer` on `stream`, then relay its bytes and
/// `connection`'s both ways, until both sides end or either fails, or
/// `revoked` resolves.
async fn relay_port(
    mut stream: UnixStream,
    (id, answer): (u64, Body),
    mut connection: TcpStream,
    revoked: impl Future<Output = ()>,
    startup: OperationGuard,
) -> Result<()> {
    protocol::write(&mut stream, &Response::new(id, answer)).await?;
    drop(startup);
    tokio::select! {
        // A connection either side resets ends as one that closes.
        _ = tokio::io::copy_bidirectional(&mut stream, &mut connection) => {}
        () = revoked => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
