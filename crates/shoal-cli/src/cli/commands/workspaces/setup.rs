//! Workspace setup: starting it, handling its failure and starting the post-setup hook.
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};

use super::recovery::{issues, reconcile};
use super::waiting::WaitingAgent;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::operations,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::recovery::ReconcileOptions,
    env, execution, git,
    model::{QueueEntry, Workspace},
    protocol::operations::{
        Failure, OperationSpec, Preparation, Prepared, QueuedLaunch, StartedWorkspace,
    },
    removal::UnsavedWork,
    state::WorkspaceState,
};

/// Copy the main checkout's ignored files but those `exclude` matches inside
/// tracked setup, then become the setup command so its exit status and
/// signals are the execution's.
pub(in crate::cli::commands) async fn copy_ignored_worker(
    exclude: &[String],
    setup_cmd: Option<PathBuf>,
) -> Result<i32> {
    ensure!(
        env::is_scoped(),
        "copy-ignored worker requires a tracked execution"
    );
    // Tracked setup runs in the workspace root.
    let workspace = std::env::current_dir()?;
    let copied = git::ignored::copy(&workspace, exclude).await?;
    if copied > 0 {
        eprintln!(
            "Copied {copied} ignored file{} from the repository checkout",
            if copied == 1 { "" } else { "s" }
        );
    }
    match setup_cmd {
        Some(setup_cmd) => Err(become_setup(&setup_cmd)),
        None => Ok(0),
    }
}

/// Replace this process with `setup_cmd`; returns only the error when that fails.
fn become_setup(setup_cmd: &Path) -> anyhow::Error {
    let error =
        std::os::unix::process::CommandExt::exec(&mut std::process::Command::new(setup_cmd));
    anyhow::Error::new(error).context(format!("run {}", setup_cmd.display()))
}

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
    prepare_settled(ctx, workspace, steps, None).await
}

/// Run `steps`, setup among them, in one operation the daemon starts, then
/// settle a failed setup as [`setup_workspace`] does, a retry running
/// `steps` again, with the agent `waiting` for it.
async fn prepare_settled(
    ctx: &Context,
    workspace: &Workspace,
    steps: Preparation,
    waiting: Option<&mut WaitingAgent>,
) -> Result<Option<SetUp>> {
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: steps.clone(),
    };
    let prepared = operations::run::<Prepared>(ctx, spec).await;
    settle_setup(ctx, workspace, prepared, Some(steps), waiting).await
}

/// Follow the preparation the daemon started for the workspace a request
/// opened, and settle it as [`setup_workspace`] does; a retried setup runs
/// alone, leaving what follows it to the caller.
pub(in crate::cli::commands) async fn follow_setup(
    ctx: &Context,
    started: StartedWorkspace,
) -> Result<Option<SetUp>> {
    let prepared = operations::follow_started::<Prepared>(ctx, started.operation).await;
    settle_setup(ctx, &started.workspace, prepared, None, None).await
}

/// The workspace a preparation left, once an interactive user decided what
/// to do with a failed setup. A retry runs `retry`, or else setup alone,
/// whose caller runs what follows it as after an ignored failure. A retry
/// A failure leaves `waiting`, the agent of the start, waiting in the queue,
/// which starts it once a setup succeeds, so a retry queues no other.
pub(in crate::cli::commands) async fn settle_setup(
    ctx: &Context,
    workspace: &Workspace,
    prepared: Result<Prepared>,
    retry: Option<Preparation>,
    waiting: Option<&mut WaitingAgent>,
) -> Result<Option<SetUp>> {
    match prepared {
        Ok(prepared) => Ok(Some(SetUp {
            workspace: *prepared.workspace,
            post_setup: Some(Ok(())),
            agent: prepared.agent,
        })),
        Err(error) => match error.downcast::<execution::SetupVerificationFailure>() {
            Ok(failure) => {
                report_setup_verification_failure(ctx, workspace, failure.exit_code).await
            }
            Err(error) if is_hook_failure(&error) => {
                let workspace = crate::client::workspaces::inspect(
                    &ctx.paths,
                    SpaceSelector::id(&workspace.id),
                )
                .await?;
                Ok(Some(SetUp {
                    workspace: workspace.workspace,
                    post_setup: Some(Err(error)),
                    agent: None,
                }))
            }
            Err(error) if after_setup(ctx, workspace).await? => Err(error),
            Err(error) => handle_setup_failure(ctx, workspace, error, retry, waiting).await,
        },
    }
}

/// Whether `workspace` is ready, so what failed came after its setup, such
/// as queuing its agent, and leaves no setup to recover.
async fn after_setup(ctx: &Context, workspace: &Workspace) -> Result<bool> {
    let inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    Ok(inspection.workspace.state == WorkspaceState::Ready)
}

/// Let an interactive user retry, delete, ignore or keep the workspace whose
/// setup failed with `error`; a workspace whose failure is ignored has not
/// run its post-setup hook, nor has one retried without `retry`, which says
/// what a retry runs. A setup that did not start leaves nothing to delete or
/// ignore. The agent `waiting` for the setup waits in the queue from now on.
async fn handle_setup_failure(
    ctx: &Context,
    workspace: &Workspace,
    error: anyhow::Error,
    mut retry: Option<Preparation>,
    mut waiting: Option<&mut WaitingAgent>,
) -> Result<Option<SetUp>> {
    if let Some(waiting) = waiting.as_deref_mut() {
        waiting.park(ctx, workspace).await;
        if waiting.failed()
            && let Some(retry) = &mut retry
        {
            retry.agent = None;
        }
    }
    let ran = !matches!(
        error.downcast_ref::<operations::Failed>(),
        Some(operations::Failed(Failure::SetupNotStarted { .. }))
    );
    let error = format!("{error:#}");
    let (name, number) = (&workspace.name, workspace.number);
    let failed = if ran {
        "Setup failed"
    } else {
        "Setup did not start"
    };
    if !ctx.interactive() {
        match ran {
            true => bail!(
                "setup failed for {name}: {error}; workspace retained. Retry with `shoal setup {number}`, skip it with `shoal setup {number} --skip`, or delete with `shoal rm {number} --yes`"
            ),
            false => bail!(
                "setup of {name} did not start: {error}; workspace retained. Retry with `shoal setup {number}`"
            ),
        }
    }
    eprintln!(
        "{} for {name}: {error}",
        Palette::stderr(ctx.json).paint(Style::Error, failed)
    );
    match ui::setup_failure_choice(ran)? {
        ui::SetupFailureChoice::Retry => return retry_setup(ctx, workspace, retry, waiting).await,
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
/// follows it to the caller, with the agent `waiting` for it.
async fn retry_setup(
    ctx: &Context,
    workspace: &Workspace,
    retry: Option<Preparation>,
    waiting: Option<&mut WaitingAgent>,
) -> Result<Option<SetUp>> {
    let steps = retry.clone().unwrap_or(Preparation {
        setup: true,
        ..Default::default()
    });
    let retried = Box::pin(prepare_settled(ctx, workspace, steps, waiting)).await?;
    Ok(retried.map(|set_up| SetUp {
        post_setup: set_up.post_setup.filter(|_| retry.is_some()),
        ..set_up
    }))
}

/// Whether the operation got past setup and failed in the post-setup hook.
fn is_hook_failure(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<operations::Failed>(),
        Some(operations::Failed(Failure::HookFailed { .. }))
    )
}

/// Process uncertainty cannot be ignored or safely stopped from the still-live
/// reporting wrapper. Keep the worktree and expose the daemon's actual evidence.
async fn report_setup_verification_failure(
    ctx: &Context,
    workspace: &Workspace,
    code: i32,
) -> Result<Option<SetUp>> {
    let inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    let (name, number) = (&workspace.name, workspace.number);
    let reason = inspection
        .workspace
        .error
        .as_deref()
        .unwrap_or("process state is unknown");
    let message = format!(
        "Setup verification incomplete for {name} (command exit {code}): {reason}\nWorkspace retained at {}. Stop its processes with `shoal stop {number}`, then retry setup with `shoal setup {number}` or skip it with `shoal setup {number} --skip`.",
        workspace.path.display()
    );
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
