//! The daemon's answers to [`DaemonMethod`] requests, and who may send them.
use anyhow::Result;

use super::super::{
    Server,
    answer::Answer,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{self, Body, DaemonMethod, DaemonStatus, ErrorCode, Response};

/// The daemon's own requests, which read how it runs.
pub(super) async fn handle(server: &Server, method: DaemonMethod) -> Result<Answer> {
    let manager = &server.manager;
    Ok(Answer::Body(match method {
        DaemonMethod::Status => Body::Status(DaemonStatus {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_secs: server.started.elapsed().as_secs(),
            managed: server.managed,
            unread_notifications: manager.unread_notifications().await?,
        }),
        DaemonMethod::Shutdown if server.managed => Body::error(
            ErrorCode::ManagedService,
            "stop the daemon through the OS service manager",
        ),
        DaemonMethod::Shutdown => {
            let shutdown = server.shutdown.clone();
            return Ok(Answer::connection(
                move |mut stream, request_id, guard| async move {
                    // The reply is written under the operation, as every reply is.
                    let _operation = guard;
                    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
                    let _ = shutdown.send(true);
                    Ok(())
                },
            ));
        }
        DaemonMethod::ReloadConfig => {
            manager.reload_config().await?;
            // The checks can outlast the reply's budget, so they run after it.
            manager.environment_recheck.notify_one();
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
