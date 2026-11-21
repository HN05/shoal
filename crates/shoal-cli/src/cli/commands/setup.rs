//! Workspace setup: starting it, handling its failure and starting the post-setup hook.
use anyhow::{Result, ensure};
use shoal_workers::preparation as unattended;

use super::recovery::{issues, reconcile};
use crate::{
    cli::{
        commands::operations,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::connection::CommandPaths,
    client::preparation::{self, Preparing, Unprepared},
    env,
    model::{QueueEntry, Workspace},
    protocol::operations::{Failure, OperationSpec, Preparation, Prepared, QueuedLaunch},
    removal::UnsavedWork,
    state::WorkspaceState,
};
use shoal_protocol::recovery::ReconcileOptions;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn setup(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace.clone()).await?;
    let Some(set_up) = setup_workspace(ctx, &inspection.workspace, true).await? else {
        return Ok(1);
    };
    let workspace = set_up.finish(ctx).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Set up {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// Accept a failed setup as it is: mark the workspace ready once its
/// worktree verifies and no setup process remains.
pub(in crate::cli::commands) async fn skip_setup(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    ensure!(
        workspace.state == WorkspaceState::Failed,
        "{} is {}; only a failed setup can be skipped",
        workspace.name,
        workspace.state
    );
    ignore_setup_failure(ctx, &workspace).await?;
    let workspace =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
            .await?
            .workspace;
    run_post_setup(ctx, &workspace).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Skipped setup of {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// Run the configured `hooks.post_setup`, if any, once the workspace is ready,
/// in an operation the daemon starts. A failure keeps the ready workspace
/// and stops what would follow.
pub(in crate::cli::commands) async fn run_post_setup(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<()> {
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            post_setup: true,
            ..Default::default()
        },
    };
    operations::run::<Prepared>(ctx, spec).await?;
    Ok(())
}

/// A workspace setup made ready, and how what follows setup in the same
/// operation went: its post-setup hook, when the operation runs it, and the
/// agent it queued.
pub(in crate::cli::commands) struct SetUp {
    pub workspace: Workspace,
    /// `None` when the user ignored setup's failure, so nothing that
    /// follows setup ran.
    pub post_setup: Option<Result<()>>,
    pub agent: Option<QueueEntry>,
}

impl SetUp {
    /// The workspace once its post-setup hook succeeded, running it now
    /// when it did not run with setup.
    pub(in crate::cli::commands) async fn finish(self, ctx: &Context) -> Result<Workspace> {
        let prepared = self.finish_queuing(ctx, None).await?;
        Ok(*prepared.workspace)
    }

    /// The prepared workspace once its post-setup hook succeeded, running
    /// it now, then queuing `agent`, when they did not run with setup.
    pub(in crate::cli::commands) async fn finish_queuing(
        self,
        ctx: &Context,
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
                operations::run::<Prepared>(ctx, spec).await
            }
        }
    }
}

/// Run the setup command, and the post-setup hook after it when
/// `post_setup`, in one operation the daemon starts, so both go on when
/// this process goes. Then let an interactive user decide what to do with
/// a failed workspace, whose hook did not run. `None` means the workspace
/// was deleted or kept as-is.
pub(in crate::cli::commands) async fn setup_workspace(
    ctx: &Context,
    workspace: &Workspace,
    post_setup: bool,
) -> Result<Option<SetUp>> {
    let steps = Preparation {
        setup: true,
        post_setup,
        ..Default::default()
    };
    prepare_settled(ctx, workspace, steps).await
}

/// Run `steps`, setup among them, in one operation the daemon starts, then
/// settle a failed setup as [`setup_workspace`] does, a retry running
/// `steps` again.
async fn prepare_settled(
    ctx: &Context,
    workspace: &Workspace,
    steps: Preparation,
) -> Result<Option<SetUp>> {
    let env = env::without_secrets();
    let preparing = preparation::prepare(&ctx.paths, workspace, steps.clone(), env).await?;
    let prepared = operations::follow_preparation(ctx, preparing).await?;
    settle_setup(ctx, prepared, Some(steps)).await
}

/// Follow the preparation the daemon started for the workspace a request
/// opened, and settle it as [`setup_workspace`] does; a retried setup runs
/// alone, leaving what follows it to the caller.
pub(in crate::cli::commands) async fn follow_setup(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Option<SetUp>> {
    let prepared = operations::follow_preparation(ctx, preparing).await?;
    settle_setup(ctx, prepared, None).await
}

/// The workspace a preparation left, once an interactive user decided what
/// to do with a failed setup. A retry runs `retry`, or else setup alone,
/// whose caller runs what follows it as after an ignored failure.
pub(in crate::cli::commands) async fn settle_setup(
    ctx: &Context,
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
    let Unprepared { workspace, failure } = unprepared;
    match failure {
        Failure::SetupUnverified { exit_code } => {
            report_setup_verification_failure(ctx, &workspace, exit_code)
        }
        // Nothing exists to ignore or delete until a retry creates it.
        failure @ Failure::WorktreeFailed { .. } => Err(crate::operations::error(failure)),
        Failure::HookFailed { .. } => Ok(Some(SetUp {
            workspace: *workspace,
            post_setup: Some(Err(crate::operations::error(failure))),
            agent: None,
        })),
        failure if set_up => Err(crate::operations::error(failure)),
        failure => handle_setup_failure(ctx, &workspace, failure, retry).await,
    }
}

/// Let an interactive user retry, delete, ignore or keep the workspace whose
/// setup failed with `failure`; a workspace whose failure is ignored has not
/// run its post-setup hook, nor has one retried without `retry`, which says
/// what a retry runs. A setup that did not start leaves nothing to delete or
/// ignore.
async fn handle_setup_failure(
    ctx: &Context,
    workspace: &Workspace,
    failure: Failure,
    retry: Option<Preparation>,
) -> Result<Option<SetUp>> {
    if !ctx.interactive() {
        return Err(unattended::unsettled(workspace, failure));
    }
    let ran = !matches!(failure, Failure::SetupNotStarted { .. });
    let error = format!("{:#}", crate::operations::error(failure));
    let name = &workspace.name;
    let failed = if ran {
        "Setup failed"
    } else {
        "Setup did not start"
    };
    eprintln!(
        "{} for {name}: {error}",
        Palette::stderr(ctx.json).paint(Style::Error, failed)
    );
    match ui::setup_failure_choice(ran)? {
        ui::SetupFailureChoice::Retry => return retry_setup(ctx, workspace, retry).await,
        ui::SetupFailureChoice::Delete => {
            delete_failed_workspace(ctx, workspace).await?;
            return Ok(None);
        }
        ui::SetupFailureChoice::Ignore => ignore_setup_failure(ctx, workspace).await?,
        ui::SetupFailureChoice::Cancel => return Ok(None),
    }
    let inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
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
    ctx: &Context,
    workspace: &Workspace,
    retry: Option<Preparation>,
) -> Result<Option<SetUp>> {
    let steps = retry.clone().unwrap_or(Preparation {
        setup: true,
        ..Default::default()
    });
    let retried = Box::pin(prepare_settled(ctx, workspace, steps)).await?;
    Ok(retried.map(|set_up| SetUp {
        post_setup: set_up.post_setup.filter(|_| retry.is_some()),
        ..set_up
    }))
}

/// Process uncertainty cannot be ignored or safely stopped from the still-live
/// reporting wrapper. Keep the worktree and expose the daemon's actual evidence.
fn report_setup_verification_failure(
    ctx: &Context,
    workspace: &Workspace,
    code: i32,
) -> Result<Option<SetUp>> {
    let message = unattended::unverified(workspace, code);
    ensure!(ctx.interactive(), "{message}");
    eprintln!(
        "{}",
        Palette::stderr(ctx.json).paint(Style::Warning, message)
    );
    Ok(None)
}

async fn delete_failed_workspace(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let confirmed = ui::confirm(
        ctx,
        &format!(
            "Delete workspace {} at {} (including setup changes)?",
            workspace.name,
            workspace.path.display(),
        ),
        "--yes",
    )?;
    if confirmed {
        crate::client::removal::remove(
            &ctx.paths,
            SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
        )
        .await?;
        eprintln!("Deleted workspace {}", workspace.name);
    }
    Ok(())
}

async fn ignore_setup_failure(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let options = ReconcileOptions {
        repair: true,
        ..Default::default()
    };
    let reports = reconcile(ctx, workspace, options).await?;
    let (name, number) = (&workspace.name, workspace.number);
    ensure!(
        reports
            .iter()
            .all(|r| r.workspace.state == WorkspaceState::Ready),
        "setup of {name} cannot be skipped: {}; stop its processes with `shoal stop {number}`, then retry",
        issues(&reports)
    );
    Ok(())
}
