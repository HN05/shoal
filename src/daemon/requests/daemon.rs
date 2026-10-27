//! The daemon's answers to [`DaemonMethod`] requests, and who may send them.
use anyhow::Result;
use tokio::{net::UnixStream, sync::watch};

use super::super::{
    Server,
    answer::{Answer, OperationGuard},
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{self, Body, DaemonMethod, DaemonStatus, ErrorCode, Response};

/// The daemon's own requests, which read how it runs.
pub(super) async fn handle(server: &Server, method: DaemonMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DaemonMethod::Status => Body::Status(status(server).await?),
        DaemonMethod::Shutdown if server.managed => Body::error(
            ErrorCode::ManagedService,
            "stop the daemon through the OS service manager",
        ),
        DaemonMethod::Shutdown => {
            let shutdown = server.shutdown.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                shut_down(stream, request_id, shutdown, guard)
            }));
        }
        DaemonMethod::ReloadConfig => {
            server.manager.reload_config_and_recheck().await?;
            Body::Ok
        }
    }))
}

impl Scoped for DaemonMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            DaemonMethod::Status => None,
            DaemonMethod::Shutdown | DaemonMethod::ReloadConfig => return Err(denied()),
        })
    }
}

/// How the daemon runs.
async fn status(server: &Server) -> Result<DaemonStatus> {
    Ok(DaemonStatus {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_secs: server.started.elapsed().as_secs(),
        managed: server.managed,
        unread_notifications: server.manager.unread_notifications().await?,
    })
}

/// Reply to the shutdown request, then shut the daemon down.
async fn shut_down(
    mut stream: UnixStream,
    request_id: u64,
    shutdown: watch::Sender<bool>,
    guard: OperationGuard,
) -> Result<()> {
    // The reply is written under the operation, as every reply is.
    let _operation = guard;
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let _ = shutdown.send(true);
    Ok(())
}
