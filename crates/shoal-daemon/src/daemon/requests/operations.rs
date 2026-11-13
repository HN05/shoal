//! The daemon's answers to [`OperationMethod`] requests, and who may send them.
use crate::client::scope_tokens::ScopeToken;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    conversions::WorkspaceConversion,
    operations::{
        Entry,
        starts::{Opening, Preparing, Split},
    },
    scope::{Caller, Scoped},
    workspace::{Manager, NewWorkspace},
};
use crate::client::{queue::QueueEntryId, space_names::SpaceSelector};
use crate::protocol::{
    Body, ErrorCode, OperationMethod, Response,
    operations::{Started, WorkspaceStart},
};

/// Whether `method` starts a workspace or an operation, whose worker an
/// exit would cut off, which an exit that waits for work in progress holds
/// back.
fn starts_work(method: &OperationMethod) -> bool {
    match method {
        OperationMethod::StartOperation { .. }
        | OperationMethod::StartWorkspace { .. }
        | OperationMethod::StartSplit { .. }
        | OperationMethod::ConvertToWorkspace { .. } => true,
        OperationMethod::FollowOperation { .. }
        | OperationMethod::CancelOperation { .. }
        | OperationMethod::ReportProgress { .. } => false,
    }
}

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: OperationMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    if starts_work(&method) {
        manager.gates.admit()?;
    }
    Ok(Answer::Body(match method {
        OperationMethod::StartOperation { spec, env } => Body::OperationStarted(Started {
            id: manager.start_operation(spec, env, caller).await?,
        }),
        OperationMethod::FollowOperation { id, after } => {
            let entry = manager.operation(&id, caller).await?;
            let follower = Follower {
                manager: manager.clone(),
                scope: scope.cloned(),
            };
            return Ok(Answer::connection(move |stream, request_id, guard| {
                follower.follow(stream, request_id, entry, after, guard)
            }));
        }
        OperationMethod::StartWorkspace {
            repository,
            mut start,
            base,
            path,
            git_profile,
            mut parent,
            options,
            issue,
            pr,
            agent,
            waiting,
            env,
            queue_entry,
        } => {
            let queued = match caller {
                Some(_) => None,
                None => queued_start(manager, queue_entry.clone(), &mut parent, &mut start).await?,
            };
            let caller = caller.or(queued.as_ref());
            let opening = Opening {
                start,
                base,
                new: NewWorkspace {
                    git_profile: git_profile.as_deref(),
                    path,
                    options,
                    ..Default::default()
                },
                parent: manager.space_id(parent.as_ref()).await?,
            };
            let preparing = Preparing {
                issue,
                pr,
                agent,
                waiting,
                queue_entry,
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
            copy,
            base,
            options,
            issue,
            agent,
            env,
        } => {
            let split = Split {
                name,
                copy,
                base,
                options,
            };
            let preparing = Preparing {
                issue,
                pr: None,
                agent,
                waiting: None,
                queue_entry: None,
                env,
            };
            Body::WorkspaceStarted(
                manager
                    .start_split(&workspace, split, preparing, caller)
                    .await?,
            )
        }
        OperationMethod::ConvertToWorkspace {
            chat,
            branch,
            issue,
            base,
            options,
            mode,
            env,
        } => {
            let conversion = WorkspaceConversion {
                branch,
                issue,
                base,
                options,
                mode: mode.unwrap_or(crate::config::modes::ModeName::DEFAULT),
            };
            Body::WorkspaceStarted(
                manager
                    .convert_to_workspace(&chat, conversion, env, caller)
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
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            OperationMethod::StartOperation { spec, .. } => Some(spec.workspace_mut()),
            OperationMethod::StartSplit {
                workspace, copy, ..
            } => {
                if let Some(copy) = copy {
                    manager.copy_within(caller, copy).await?;
                }
                Some(workspace)
            }
            OperationMethod::StartWorkspace {
                parent,
                start,
                queue_entry,
                ..
            } => {
                confine_start(manager, caller, parent, start).await?;
                if let Some(entry) = queue_entry {
                    manager.ensure_own_launch(caller, entry, start).await?;
                }
                None
            }
            OperationMethod::ConvertToWorkspace { chat, .. } => Some(chat),
            OperationMethod::FollowOperation { id, .. }
            | OperationMethod::CancelOperation { id }
            | OperationMethod::ReportProgress { id, .. } => {
                manager.operation(id, Some(caller)).await?;
                None
            }
        })
    }
}

/// The scope the user's process opens a workspace in for the launch of
/// queue entry `entry`: that of the space whose scoped process queued it,
/// to which the start is confined, or the user's own for any other.
async fn queued_start(
    manager: &Manager,
    entry: Option<QueueEntryId>,
    parent: &mut Option<SpaceSelector>,
    start: &mut WorkspaceStart,
) -> Result<Option<Caller>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    let Some(queued) = manager.queued_scope(&entry).await? else {
        return Ok(None);
    };
    confine_start(manager, &queued, parent, start).await?;
    Ok(Some(queued))
}

/// Confine a workspace start of `caller` to its own tree: below its own
/// space unless `parent` names one below it, from a copy of a workspace it
/// reaches, or reopening a workspace below its own, which keeps its place.
async fn confine_start(
    manager: &Manager,
    caller: &Caller,
    parent: &mut Option<SpaceSelector>,
    start: &mut WorkspaceStart,
) -> Result<()> {
    match start {
        WorkspaceStart::Workspace(workspace) => {
            return manager.reopened_below(caller, workspace).await;
        }
        WorkspaceStart::Copy { source, .. } => manager.copy_within(caller, source).await?,
        _ => {}
    }
    manager.parent_below(caller, parent).await
}

/// Who follows an operation: the scope token it sent, which must stay
/// valid for as long as it follows.
struct Follower {
    manager: Arc<Manager>,
    scope: Option<ScopeToken>,
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
            let (events, ended) = entry.events_from(next);
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
