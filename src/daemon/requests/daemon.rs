//! The daemon's answers to [`DaemonMethod`] and exit requests, and who may
//! send them.
use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    Server,
    answer::{Answer, OperationGuard},
    scope::{self, Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{self, Body, DaemonMethod, DaemonStatus, ErrorCode, ExitRequest, Response};

/// The daemon's own requests, which read how it runs.
pub(super) async fn handle(server: &Server, method: DaemonMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DaemonMethod::Status => Body::Status(status(server).await?),
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
            DaemonMethod::ReloadConfig => return Err(denied()),
        })
    }
}

impl Scoped for ExitRequest {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Err(denied())
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

/// Reply to an exit request the caller's scope allows, then end the daemon
/// with what the request asks of running work. The daemon ends even when the
/// client hung up first.
pub(in crate::daemon) async fn exit(
    mut stream: UnixStream,
    request_id: u64,
    server: &Server,
    scope: Option<&str>,
    mut request: ExitRequest,
    guard: OperationGuard,
) -> Result<()> {
    // The reply is written under the operation, as every reply is.
    let _operation = guard;
    let authorized = scope::authorize(&server.manager, scope, &mut request).await;
    let body = match &authorized {
        Ok(_) => Body::Ok,
        Err(error) => Body::error(ErrorCode::ScopeDenied, format!("{error:#}")),
    };
    let replied = protocol::write(&mut stream, &Response::new(request_id, body)).await;
    if authorized.is_ok() {
        // A full channel already holds an earlier request, which wins.
        let _ = server.exit.try_send(request.exit.into());
    }
    replied
}
