//! Running a workspace's setup and post-setup hook in an operation.
use anyhow::{Result, ensure};
use shoal_protocol::{hub::Machine, recovery::ReconcileOptions, space_names::SpaceSelector};

use super::{FailedSetup, Frontend, Settling, follow, unsettled, unverified};
use crate::{
    client::{
        connection::CommandPaths,
        preparation::{self, Preparing, Unprepared},
    },
    following,
    model::{QueueEntry, Workspace},
    operations::error,
    protocol::operations::{Failure, OperationSpec, Preparation, Prepared, QueuedLaunch},
    state::WorkspaceState,
};
use shoal_core::removal::UnsavedWork;

/// A workspace setup made ready, and how what follows setup in the same
/// operation went: its post-setup hook, when the operation runs it, and the
/// agent it queued.
pub struct SetUp {
    pub workspace: Workspace,
    /// `None` when the user ignored setup's failure, so nothing that
    /// follows setup ran.
    pub post_setup: Option<Result<()>>,
    pub agent: Option<QueueEntry>,
}

impl SetUp {
    /// The workspace once its post-setup hook succeeded, running it now
    /// when it did not run with setup.
    pub async fn finish(self, paths: &CommandPaths, frontend: &impl Frontend) -> Result<Workspace> {
        let prepared = self.finish_queuing(paths, frontend, None).await?;
        Ok(*prepared.workspace)
    }

    /// The prepared workspace once its post-setup hook succeeded, running
    /// it now, then queuing `agent`, when they did not run with setup.
    pub async fn finish_queuing(
        self,
        paths: &CommandPaths,
        frontend: &impl Frontend,
        agent: Option<Box<QueuedLaunch>>,
    ) -> Result<Prepared> {
        match self.post_setup {
            Some(result) => result.map(|()| Prepared {
                workspace: Box::new(self.workspace),
                agent: self.agent,
            }),
            None => {
                let spec = OperationSpec::Prepare {
                    workspace: SpaceSelector::id(&self.workspace.id),
                    steps: Preparation {
                        post_setup: true,
                        agent,
                        ..Default::default()
                    },
                };
                run(paths, frontend, spec).await
            }
        }
    }
}

/// Run the configured `hooks.post_setup`, if any, once the workspace is ready,
/// in an operation the daemon starts. A failure keeps the ready workspace
/// and stops what would follow.
pub async fn run_post_setup(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    workspace: &Workspace,
) -> Result<()> {
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            post_setup: true,
            ..Default::default()
        },
    };
    run::<Prepared>(paths, frontend, spec).await?;
    Ok(())
}

/// Start `spec` and follow it to its end below `frontend`'s display.
async fn run<T>(paths: &CommandPaths, frontend: &impl Frontend, spec: OperationSpec) -> Result<T>
where
    T: TryFrom<crate::protocol::operations::Outcome, Error = anyhow::Error>,
{
    let mut display = frontend.display();
    following::run(paths, frontend.json(), &mut display, spec).await
}

/// Run the setup command, and the post-setup hook after it when
/// `post_setup`, in one operation the daemon starts, so both go on when
/// this process goes. Then settle a failed workspace, whose hook did not
/// run, as `frontend` decides. `None` means the workspace was deleted or
/// kept as it is.
pub async fn setup_workspace(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    workspace: &Workspace,
    post_setup: bool,
) -> Result<Option<SetUp>> {
    let steps = Preparation {
        setup: true,
        post_setup,
        ..Default::default()
    };
    prepare_settled(paths, frontend, workspace, steps).await
}

/// Run `steps`, setup among them, in one operation the daemon starts, then
/// settle a failed setup as [`setup_workspace`] does, a retry running
/// `steps` again.
async fn prepare_settled(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    workspace: &Workspace,
    steps: Preparation,
) -> Result<Option<SetUp>> {
    let env = crate::env::without_secrets();
    let preparing = preparation::prepare(paths, workspace, steps.clone(), env).await?;
    let prepared = follow(paths, frontend, preparing).await?;
    settle_setup(paths, frontend, prepared, Some(steps)).await
}

/// Follow the preparation the daemon started for the workspace a request
/// opened, and settle it as [`setup_workspace`] does; a retried setup runs
/// alone, leaving what follows it to the caller.
pub async fn follow_setup(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Option<SetUp>> {
    let prepared = follow(paths, frontend, preparing).await?;
    settle_setup(paths, frontend, prepared, None).await
}

/// The workspace a preparation left, once `frontend` decided what to do
/// with a failed setup. A retry runs `retry`, or else setup alone, whose
/// caller runs what follows it as after an ignored failure.
pub async fn settle_setup(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    prepared: Result<Prepared, Unprepared>,
    retry: Option<Preparation>,
) -> Result<Option<SetUp>> {
    let unprepared = match prepared {
        Ok(prepared) => {
            return Ok(Some(SetUp {
                workspace: *prepared.workspace,
                post_setup: Some(Ok(())),
                agent: prepared.agent,
            }));
        }
        Err(unprepared) => unprepared,
    };
    let set_up = unprepared.set_up();
    let Unprepared {
        workspace,
        machine,
        failure,
    } = unprepared;
    match failure {
        Failure::SetupUnverified { exit_code } => {
            frontend.unverified(unverified(&workspace, exit_code))?;
            Ok(None)
        }
        // Nothing exists to ignore or delete until a retry creates it.
        failure @ Failure::WorktreeFailed { .. } => Err(error(failure)),
        Failure::HookFailed { .. } => Ok(Some(SetUp {
            workspace: *workspace,
            post_setup: Some(Err(error(failure))),
            agent: None,
        })),
        failure if set_up => Err(error(failure)),
        failure => {
            let machine = machine.as_ref();
            settle_failure(paths, frontend, &workspace, machine, failure, retry).await
        }
    }
}

/// Retry, delete, ignore or keep the workspace whose setup failed with
/// `failure`, as `frontend` decides; `machine` names another machine that
/// runs it. Where the frontend cannot decide, fail and name the commands
/// that do. A workspace whose failure is ignored has not run its
/// post-setup hook, and neither has one retried without `retry`, which
/// says what a retry runs. A setup that did not start leaves nothing to
/// delete or ignore.
async fn settle_failure(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    workspace: &Workspace,
    machine: Option<&Machine>,
    failure: Failure,
    retry: Option<Preparation>,
) -> Result<Option<SetUp>> {
    let reason = format!("{:#}", error(failure.clone()));
    let failed = FailedSetup {
        workspace,
        machine,
        error: &reason,
        ran: !matches!(failure, Failure::SetupNotStarted { .. }),
    };
    let Some(settling) = frontend.settle(&failed)? else {
        return Err(unsettled(workspace, failure));
    };
    match settling {
        Settling::Retry => return retry_setup(paths, frontend, workspace, retry).await,
        Settling::Delete => {
            delete_failed(paths, workspace).await?;
            return Ok(None);
        }
        Settling::Ignore => ignore_failure(paths, workspace).await?,
        Settling::Cancel => return Ok(None),
    }
    let inspection = crate::client::workspaces::inspect(SpaceSelector::id(&workspace.id))
        .send(paths)
        .await?;
    ensure!(
        inspection.workspace.state == WorkspaceState::Ready,
        "workspace setup did not complete"
    );
    Ok(Some(SetUp {
        workspace: inspection.workspace,
        post_setup: None,
        agent: None,
    }))
}

/// Run setup again, with `retry` when given, or else alone, leaving what
/// follows it to the caller.
async fn retry_setup(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    workspace: &Workspace,
    retry: Option<Preparation>,
) -> Result<Option<SetUp>> {
    let steps = retry.clone().unwrap_or(Preparation {
        setup: true,
        ..Default::default()
    });
    let retried = Box::pin(prepare_settled(paths, frontend, workspace, steps)).await?;
    Ok(retried.map(|set_up| SetUp {
        post_setup: set_up.post_setup.filter(|_| retry.is_some()),
        ..set_up
    }))
}

async fn delete_failed(paths: &CommandPaths, workspace: &Workspace) -> Result<()> {
    let selector = SpaceSelector::id(&workspace.id);
    crate::client::removal::remove(paths, selector, UnsavedWork::All).await?;
    eprintln!("Deleted workspace {}", workspace.name);
    Ok(())
}

/// Accept a failed setup as it is: mark the workspace ready once its
/// worktree verifies and no setup process remains.
pub async fn ignore_failure(paths: &CommandPaths, workspace: &Workspace) -> Result<()> {
    let options = ReconcileOptions {
        repair: true,
        ..Default::default()
    };
    let selector = Some(SpaceSelector::id(&workspace.id));
    let reports = crate::client::recovery::reconcile(paths, selector, options).await?;
    let (name, number) = (&workspace.name, workspace.number);
    let issues = shoal_protocol::recovery::Report::issues_of(&reports);
    ensure!(
        reports
            .iter()
            .all(|r| r.workspace.state == WorkspaceState::Ready),
        "setup of {name} cannot be skipped: {issues}; stop its processes with `shoal stop {number}`, then retry"
    );
    Ok(())
}
