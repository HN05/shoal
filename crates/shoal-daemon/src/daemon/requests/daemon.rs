//! The daemon's answers to [`DaemonMethod`] and exit requests, and who may
//! send them.
use crate::client::scope_tokens::ScopeToken;
use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    Server,
    answer::{self, Answer, OperationGuard},
    exit_wait::{self, Quiet},
    permissions::Action,
    scope::{self, Caller, Scoped},
    workspace::{Ending, Manager},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionKey,
    protocol::{
        self, Body, DaemonMethod, DaemonStatus, Exit, ExitRequest, ExitWhen, Exited, Response,
    },
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

/// Stopping or restarting the daemon is the `daemon` permission's.
impl Scoped for ExitRequest {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        let action = Action::new(PermissionKey::Daemon, exit_command(self));
        manager.authorize_action(Some(caller), action).await?;
        Ok(None)
    }
}

/// The command that sends `request`, which an approval of it names.
fn exit_command(request: &ExitRequest) -> String {
    let exit = match request.exit {
        Exit::Stop => "stop",
        Exit::Restart => "restart",
    };
    let when = match request.when {
        ExitWhen::Now => " --force",
        ExitWhen::Idle => "",
        ExitWhen::IfIdle => " --no-wait",
    };
    format!("shoal daemon {exit}{when}")
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
/// work has ended. An exit that waits first holds back new work until
/// nothing it would cut off runs, telling the client what runs meanwhile,
/// and ends nothing once the client hangs up or `when` refuses to wait. The
/// daemon ends even when the client hung up after its reply.
pub(in crate::daemon) async fn exit(
    mut stream: UnixStream,
    request_id: u64,
    server: &Server,
    scope: Option<&ScopeToken>,
    mut request: ExitRequest,
    guard: OperationGuard,
) -> Result<()> {
    if let Err(error) = scope::authorize(&server.manager, scope, &mut request).await {
        let body = answer::refusal(&error);
        // The reply is written under the operation, as every reply is.
        return protocol::write(&mut stream, &Response::new(request_id, body)).await;
    }
    let (quiet, _operation) = match request.when {
        ExitWhen::Now => (None, Some(guard)),
        when => {
            // The exit's own request would keep the daemon from quiescing.
            drop(guard);
            let waiting =
                exit_wait::wait(&server.manager, &mut stream, request_id, request.exit, when);
            match waiting.await? {
                Some(quiet) => (Some(quiet), None),
                None => return Ok(()),
            }
        }
    };
    let replied = protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await;
    let requested = RequestedExit {
        ending: request.exit.into(),
        stream,
        request_id,
        quiet,
    };
    // A full channel already holds an earlier request, which wins.
    let _ = server.exit.try_send(requested);
    replied
}

/// An exit a client asked for, with the connection that hears what the
/// daemon left once its work has ended, and for an exit that waited, the
/// work it holds off until the daemon has exited.
pub(in crate::daemon) struct RequestedExit {
    pub ending: Ending,
    stream: UnixStream,
    request_id: u64,
    quiet: Option<Quiet>,
}

impl RequestedExit {
    /// Let the work a stop ends take operations, which the exit's wait held
    /// off, once the daemon accepts no more requests: stopped executions
    /// record their ends under one. A restart keeps them held off, so an
    /// execution that changes meanwhile detaches for the next daemon, as for
    /// an update.
    pub fn ending_work(&mut self) {
        if let (Ending::Stop, Some(quiet)) = (self.ending, &mut self.quiet) {
            quiet.release_operations();
        }
    }

    /// Tell the client whether ending work left stopped work for `shoal
    /// resume`, after the reply to its request, which every release reads
    /// alone.
    pub async fn answer(&mut self, manager: &Manager) {
        let exited = Exited {
            stopped_work: crate::execution::recovery::any_pending(&manager.paths),
        };
        let response = Response::new(self.request_id, Body::Exited(exited));
        // The client may have hung up.
        let _ = protocol::write(&mut self.stream, &response).await;
    }
}
