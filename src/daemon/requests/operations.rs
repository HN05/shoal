//! The daemon's answers to [`OperationMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    operations::Entry,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, OperationMethod, Response, operations::Started};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: OperationMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        OperationMethod::StartOperation { spec, env } => Body::OperationStarted(Started {
            id: manager.start_operation(spec, env, caller).await?,
        }),
        OperationMethod::FollowOperation { id, after } => {
            let entry = manager.operation(&id, caller).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                follow(stream, request_id, manager, entry, after, guard)
            }));
        }
        OperationMethod::CancelOperation { id } => {
            manager.operation(&id, caller).await?.interrupt();
            Body::Ok
        }
        OperationMethod::ReportProgress { id, report } => {
            manager.report_operation(&id, report, caller).await?;
            Body::Ok
        }
    }))
}

/// A scoped caller starts operations in its own tree, and reaches only the
/// operations of that tree.
impl Scoped for OperationMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            OperationMethod::StartOperation { spec, .. } => Some(spec.workspace_mut()),
            OperationMethod::FollowOperation { id, .. }
            | OperationMethod::CancelOperation { id }
            | OperationMethod::ReportProgress { id, .. } => {
                manager.operation(id, Some(caller)).await?;
                None
            }
        })
    }
}

/// Send the operation's events from the `after`th on as they come, then its
/// end, unless the client hangs up first. It writes only under an operation
/// guard, so an update closes it between writes.
async fn follow(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    entry: Arc<Entry>,
    after: u64,
    startup: OperationGuard,
) -> Result<()> {
    let mut changed = entry.watch();
    crate::protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut next = after;
    loop {
        changed.borrow_and_update();
        let (events, ended) = entry.since(next);
        let Some(operation) = manager.gates.try_operation() else {
            return Ok(());
        };
        for event in events {
            next = event.index + 1;
            let response = Response::new(request_id, Body::OperationEvent(event));
            crate::protocol::write(&mut writer, &response).await?;
        }
        if let Some(ended) = ended {
            let response = Response::new(request_id, Body::OperationEnded(ended));
            return crate::protocol::write(&mut writer, &response).await;
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            changed = changed.changed() => changed?,
            // The client never writes; a read completes only when it hangs up.
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}
