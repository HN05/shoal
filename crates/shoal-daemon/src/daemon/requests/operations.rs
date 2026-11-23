//! The daemon's answers to [`OperationMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use shoal_protocol::scope_tokens::ScopeToken;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    manager::Manager,
    moves::MoveStart,
    operations::{
        Entry,
        starts::{Child, Opening, Preparing},
    },
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
    workspace::NewWorkspace,
};
use crate::protocol::{
    Body, ErrorCode, OperationMethod, Response,
    operations::{OperationSpec, Started},
};
use shoal_protocol::space_names::SpaceSelector;

/// Whether `method` starts a workspace or an operation, whose worker an
/// exit would cut off, which an exit that waits for work in progress holds
/// back.
fn starts_work(method: &OperationMethod) -> bool {
    match method {
        OperationMethod::StartOperation { .. }
        | OperationMethod::StartMove { .. }
        | OperationMethod::StartWorkspace { .. }
        | OperationMethod::StartCreation { .. }
        | OperationMethod::StartChild { .. } => true,
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
        OperationMethod::StartOperation { spec, env } => {
            manager.admit_explicit_preparation(&spec).await?;
            Body::OperationStarted(Started {
                id: manager.start_operation(spec, env, caller).await?,
            })
        }
        OperationMethod::FollowOperation { operation, after } => {
            let entry = manager.named_operation(&operation, caller).await?;
            // Another machine's follow is checked there as it goes, its
            // token here standing for its caller only while it starts.
            let scope = scope.filter(|token| manager.hub.vouched_caller(token).is_none());
            let follower = Follower {
                manager: manager.clone(),
                scope: scope.cloned(),
            };
            return Ok(Answer::connection(move |stream, request_id, guard| {
                follower.follow(stream, request_id, (entry, Body::Ok), after, guard)
            }));
        }
        OperationMethod::StartMove { order, env } => {
            let id = match manager.start_move(*order, env, caller).await? {
                MoveStart::Started(id) => id,
                MoveStart::AfterTurn(deferred) => {
                    return Ok(Answer::Body(Body::MoveAfterTurn(deferred)));
                }
            };
            let entry = manager.operation(&id, caller).await?;
            // Another machine's move is followed there as it goes, its
            // token here standing for its caller only while it starts.
            let scope = scope.filter(|token| manager.hub.vouched_caller(token).is_none());
            let follower = Follower {
                manager: manager.clone(),
                scope: scope.cloned(),
            };
            let started = Body::OperationStarted(Started { id });
            return Ok(Answer::connection(move |stream, request_id, guard| {
                follower.follow(stream, request_id, (entry, started), 0, guard)
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
            waiting,
            blockers,
            env,
            queue_entry,
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
                parent: manager.space_id(parent.as_ref()).await?,
            };
            let preparing = Preparing {
                issue,
                pr,
                agent,
                waiting,
                queue_entry,
                blockers,
                unfollowed: false,
                env,
            };
            Body::WorkspaceStarted(
                manager
                    .start_workspace(repository, opening, preparing, caller)
                    .await?,
            )
        }
        OperationMethod::StartCreation { launch, below, env } => Body::WorkspaceStarted(
            manager
                .create_placed_workspace(*launch, (below, env), caller)
                .await?,
        ),
        OperationMethod::StartChild {
            parent,
            name,
            copy,
            base,
            options,
            issue,
            agent,
            env,
        } => {
            let split = Child {
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
                blockers: Vec::new(),
                unfollowed: false,
                env,
            };
            Body::WorkspaceStarted(
                manager
                    .start_child(&parent, split, preparing, caller)
                    .await?,
            )
        }
        OperationMethod::CancelOperation { operation } => {
            (manager.named_operation(&operation, caller).await?).interrupt();
            Body::Ok
        }
        OperationMethod::ReportProgress { id, report } => {
            manager.report_operation(&id, report, caller).await?;
            Body::Ok
        }
    }))
}

/// What the operation `spec` requires of the workspace it works in: its
/// setup's state, or for one that runs there, a ready workspace, which it
/// waits for while another lifecycle operation passes.
fn operation_check(spec: &OperationSpec) -> Option<TargetCheck> {
    match spec {
        // One that only runs the post-setup hook starts no setup there.
        OperationSpec::Prepare { steps, .. } => Some(match steps.setup {
            true => TargetCheck::SetupWorkspace,
            false => TargetCheck::Workspace,
        }),
        // Opening a PR pushes through a command run, as a push does.
        OperationSpec::Merge { .. }
        | OperationSpec::Unmerge { .. }
        | OperationSpec::Push { .. }
        | OperationSpec::OpenPull { .. } => Some(TargetCheck::AwaitedReadySpace),
        OperationSpec::Move { .. } | OperationSpec::FinishMove { .. } => None,
    }
}

impl Checked for OperationMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::FollowOperation { .. } | Self::CancelOperation { .. } => Some(TargetCheck::Space),
            // Its spec names its workspace, which only a picker judges.
            Self::StartOperation { spec, .. } => operation_check(spec),
            Self::StartWorkspace { .. }
            | Self::StartCreation { .. }
            | Self::StartChild { .. }
            | Self::ReportProgress { .. }
            | Self::StartMove { .. } => None,
        }
    }
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
            OperationMethod::StartOperation { spec, .. } => spec.workspace_mut(),
            OperationMethod::StartMove { order, .. } => {
                manager.authorize_move(caller, order).await?;
                None
            }
            OperationMethod::StartChild { parent, copy, .. } => {
                if let Some(copy) = copy {
                    manager
                        .coordination
                        .copy_within(manager, caller, copy)
                        .await?;
                }
                Some(parent)
            }
            // A creation confines what it places to the caller's tree, as a
            // launch does.
            OperationMethod::StartCreation { launch, .. } => {
                manager.select_launch_spaces(launch).await?;
                None
            }
            OperationMethod::StartWorkspace {
                parent,
                start,
                queue_entry,
                ..
            } => {
                manager
                    .coordination
                    .confine_workspace_start(manager, caller, parent, start)
                    .await?;
                if let Some(entry) = queue_entry {
                    manager
                        .coordination
                        .ensure_own_launch(manager, caller, entry, start)
                        .await?;
                }
                None
            }
            // One this machine runs is checked here; one named with a space
            // is that space's, wherever it runs.
            OperationMethod::FollowOperation { operation, .. }
            | OperationMethod::CancelOperation { operation } => match &mut operation.space {
                Some(space) if !manager.has_operation(&operation.id) => Some(space),
                _ => {
                    manager.operation(&operation.id, Some(caller)).await?;
                    None
                }
            },
            OperationMethod::ReportProgress { id, .. } => {
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
    scope: Option<ScopeToken>,
}

impl Follower {
    /// Answer with `first`, then send the operation's events from the
    /// `after`th on as they come, then its end, unless the client hangs up
    /// first or its scope token is revoked. It writes only under an
    /// operation guard, so an update closes it between writes.
    async fn follow(
        self,
        mut stream: UnixStream,
        request_id: u64,
        (entry, first): (Arc<Entry>, Body),
        after: u64,
        startup: OperationGuard,
    ) -> Result<()> {
        let manager = &self.manager;
        let mut changed = entry.watch();
        crate::protocol::write(&mut stream, &Response::new(request_id, first)).await?;
        drop(startup);
        let (mut reader, mut writer) = stream.split();
        let mut next = after;
        loop {
            changed.borrow_and_update();
            // Read the events before the scope, so an event pushed after the
            // scope ended is read only by a lookup that sees it ended.
            let (events, ended) = entry.events_from(next);
            let scoped = match &self.scope {
                Some(token) => manager
                    .coordination
                    .caller(&**manager, token)
                    .await?
                    .is_some(),
                None => true,
            };
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
