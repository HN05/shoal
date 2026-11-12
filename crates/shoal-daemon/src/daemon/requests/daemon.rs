//! The daemon's answers to [`DaemonMethod`] and exit requests, and who may
//! send them.
use crate::client::scope_tokens::ScopeToken;
use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    Server,
    answer::{Answer, OperationGuard},
    scope::{self, Caller, Scoped, denied},
    workspace::{Ending, Manager},
};
use crate::client::space_names::SpaceSelector;
use crate::protocol::{
    self, Body, DaemonMethod, DaemonStatus, ErrorCode, ExitRequest, Exited, Response,
};

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
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        // Reloading applies the config on disk and stops nothing, so a
        // setup agent may apply the config it wrote.
        Ok(match self {
            DaemonMethod::Status | DaemonMethod::ReloadConfig => None,
        })
    }
}

impl Scoped for ExitRequest {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
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
        access_stalls: server.manager.file_access.stalls(),
        cleanup_error: server.manager.cleanup_problem(),
    })
}

/// Reply to an exit request the caller's scope allows, then end the daemon
/// with what the request asks of running work, which answers again once that
/// work has ended. The daemon ends even when the client hung up first.
pub(in crate::daemon) async fn exit(
    mut stream: UnixStream,
    request_id: u64,
    server: &Server,
    scope: Option<&ScopeToken>,
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
        let requested = RequestedExit {
            ending: request.exit.into(),
            stream,
            request_id,
        };
        // A full channel already holds an earlier request, which wins.
        let _ = server.exit.try_send(requested);
    }
    replied
}

/// An exit a client asked for, with the connection that hears what the
/// daemon left once its work has ended.
pub(in crate::daemon) struct RequestedExit {
    pub ending: Ending,
    stream: UnixStream,
    request_id: u64,
}

impl RequestedExit {
    /// Tell the client whether ending work left stopped work for `shoal
    /// resume`, after the reply to its request, which every release reads
    /// alone.
    pub async fn answer(mut self, manager: &Manager) {
        let exited = Exited {
            stopped_work: crate::execution::recovery::any_pending(&manager.paths),
        };
        let response = Response::new(self.request_id, Body::Exited(exited));
        // The client may have hung up.
        let _ = protocol::write(&mut self.stream, &response).await;
    }
}
