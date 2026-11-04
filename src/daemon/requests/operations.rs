//! The daemon's answers to [`OperationMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    operations::{
        Entry,
        starts::{Opening, Preparing},
    },
    scope::{Caller, Scoped},
    workspace::{Manager, NewWorkspace},
};
use crate::protocol::{Body, ErrorCode, OperationMethod, Response, operations::Started};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: OperationMethod,
    caller: Option<&Caller>,
    scope: Option<&str>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        OperationMethod::StartOperation { spec, env } => Body::OperationStarted(Started {
            id: manager.start_operation(spec, env, caller).await?,
        }),
        OperationMethod::FollowOperation { id, after } => {
            let entry = manager.operation(&id, caller).await?;
            let follower = Follower {
                manager: manager.clone(),
                scope: scope.map(str::to_owned),
            };
            return Ok(Answer::connection(move |stream, request_id, guard| {
                follower.follow(stream, request_id, entry, after, guard)
            }));
        }
        OperationMethod::StartWorkspace {
            repository,
            start,
            base,
            path,
            git_profile,
            parent,
            options,
            issue,
            pr,
            agent,
            env,
        } => {
            let opening = Opening {
                start,
                base,
                new: NewWorkspace {
                    git_profile: git_profile.as_deref(),
                    path,
                    options,
                    ..Default::default()
                },
                parent,
            };
            let preparing = Preparing {
                issue,
                pr,
                agent,
                env,
            };
            Body::WorkspaceStarted(
                manager
                    .start_workspace(repository, opening, preparing, caller)
                    .await?,
            )
        }
        OperationMethod::StartSplit {
            workspace,
            name,
            base,
            options,
            issue,
            agent,
            env,
        } => {
            let preparing = Preparing {
                issue,
                pr: None,
                agent,
                env,
            };
            Body::WorkspaceStarted(
                manager
                    .start_split(&workspace, name, base, options, preparing, caller)
                    .await?,
            )
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
            OperationMethod::StartSplit { workspace, .. } => Some(workspace),
            // A scoped caller opens workspaces only below its own space,
            // which must be in their repository.
            OperationMethod::StartWorkspace { parent, .. } => {
                *parent = Some(caller.space_id.clone());
                None
            }
            OperationMethod::FollowOperation { id, .. }
            | OperationMethod::CancelOperation { id }
            | OperationMethod::ReportProgress { id, .. } => {
                manager.operation(id, Some(caller)).await?;
                None
            }
        })
    }
}

/// Who follows an operation: the scope token it sent, which must stay
/// valid for as long as it follows.
struct Follower {
    manager: Arc<Manager>,
    scope: Option<String>,
}

impl Follower {
    /// Send the operation's events from the `after`th on as they come, then
    /// its end, unless the client hangs up first or its scope token is
    /// revoked. It writes only under an operation guard, so an update closes
    /// it between writes.
    async fn follow(
        self,
        mut stream: UnixStream,
        request_id: u64,
        entry: Arc<Entry>,
        after: u64,
        startup: OperationGuard,
    ) -> Result<()> {
        let manager = &self.manager;
        let mut changed = entry.watch();
        crate::protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        drop(startup);
        let (mut reader, mut writer) = stream.split();
        let mut next = after;
        loop {
            changed.borrow_and_update();
            let scoped = match &self.scope {
                Some(token) => manager.caller(token).await?.is_some(),
                None => true,
            };
            let (events, ended) = entry.since(next);
            let Some(operation) = manager.gates.try_operation() else {
                return Ok(());
            };
            if !scoped {
                let body =
                    Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
                let response = Response::new(request_id, body);
                return crate::protocol::write(&mut writer, &response).await;
            }
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
}
