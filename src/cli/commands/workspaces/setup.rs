//! Workspace setup: running it, handling its failure and running the post-setup hook.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use super::{
    environment::configured_environment,
    recovery::{issues, reconcile},
};
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::recovery::ReconcileOptions,
    env, execution, git,
    hooks::{self, Hook, HookKind},
    model::Workspace,
    protocol::{RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    state::WorkspaceState,
};

/// Copy the main checkout's ignored files inside tracked setup, then become
/// the setup command so its exit status and signals are the execution's.
pub(in crate::cli::commands) async fn copy_ignored_worker(
    ctx: &Context,
    setup_cmd: Option<PathBuf>,
) -> Result<i32> {
    ensure!(
        env::is_scoped(),
        "copy-ignored worker requires a tracked execution"
    );
    // Tracked setup runs in the workspace root.
    let workspace = std::env::current_dir()?;
    let copied = git::worktrunk::copy_ignored(&ctx.paths.worktrunk_config(), &workspace).await?;
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
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let Some(workspace) = setup_workspace(ctx, &inspection.workspace).await? else {
        return Ok(1);
    };
    run_post_setup(ctx, &workspace).await?;
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
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    ensure!(
        workspace.state == WorkspaceState::Failed,
        "{} is {}; only a failed setup can be skipped",
        workspace.name,
        workspace.state
    );
    ignore_setup_failure(ctx, &workspace).await?;
    let workspace = client::inspect(&ctx.paths, workspace.id).await?.workspace;
    run_post_setup(ctx, &workspace).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Skipped setup of {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// Run the configured `post_setup_cmd`, if any, once the workspace is ready.
/// A failure keeps the ready workspace and stops what would follow.
pub(in crate::cli::commands) async fn run_post_setup(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<()> {
    let command = request::<Option<PathBuf>>(
        &ctx.paths,
        WorkspaceMethod::WorkspaceHook {
            workspace: workspace.id.clone(),
            kind: HookKind::PostSetup,
        },
    )
    .await?;
    if let Some(command) = command {
        let configured_env = configured_environment(ctx, workspace).await?;
        hooks::run_interactive(
            Hook::PostSetup,
            workspace,
            &command,
            &ctx.paths,
            ctx.json,
            &configured_env,
        )
        .await?;
    }
    Ok(())
}

/// Run the setup command, then let an interactive user decide what to do with
/// a failed workspace. `None` means the workspace was deleted or kept as-is.
pub(in crate::cli::commands) async fn setup_workspace(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<Option<Workspace>> {
    let result = execution::setup(&ctx.paths, workspace.id.clone(), ctx.json).await;
    if let Err(error) = &result
        && let Some(failure) = error.downcast_ref::<execution::SetupVerificationFailure>()
    {
        return report_setup_verification_failure(ctx, workspace, failure.exit_code).await;
    }
    let failure = match result {
        Ok(0) => None,
        Ok(code) => Some(format!("setup command exited with status {code}")),
        Err(error) => Some(format!("{error:#}")),
    };
    if let Some(error) = failure {
        let name = &workspace.name;
        ensure!(
            ctx.interactive(),
            "setup failed for {name}: {error}; workspace retained. Retry with `shoal work setup {name}`, skip it with `shoal work setup {name} --skip`, or delete with `shoal rm {name} --yes`"
        );
        eprintln!(
            "{} for {name}: {error}",
            Palette::stderr(ctx.json).paint(Style::Error, "Setup failed")
        );
        match ui::setup_failure_choice()? {
            ui::SetupFailureChoice::Delete => {
                delete_failed_workspace(ctx, workspace).await?;
                return Ok(None);
            }
            ui::SetupFailureChoice::Ignore => ignore_setup_failure(ctx, workspace).await?,
            ui::SetupFailureChoice::Cancel => return Ok(None),
        }
    }
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    ensure!(
        inspection.workspace.state == WorkspaceState::Ready,
        "workspace setup did not complete"
    );
    Ok(Some(inspection.workspace))
}

/// Process uncertainty cannot be ignored or safely stopped from the still-live
/// reporting wrapper. Keep the worktree and expose the daemon's actual evidence.
async fn report_setup_verification_failure(
    ctx: &Context,
    workspace: &Workspace,
    code: i32,
) -> Result<Option<Workspace>> {
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    let name = &workspace.name;
    let reason = inspection
        .workspace
        .error
        .as_deref()
        .unwrap_or("process state is unknown");
    let message = format!(
        "Setup verification incomplete for {name} (command exit {code}): {reason}\nWorkspace retained at {}. Stop its processes with `shoal stop {name}`, then retry setup with `shoal work setup {name}` or skip it with `shoal work setup {name} --skip`.",
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
        request::<RemovalResult>(
            &ctx.paths,
            RemovalMethod::RemoveWorkspace {
                workspace: workspace.id.clone(),
                unsaved: UnsavedWork::All,
            },
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
    let name = &workspace.name;
    ensure!(
        reports
            .iter()
            .all(|r| r.workspace.state == WorkspaceState::Ready),
        "setup of {name} cannot be skipped: {}; stop its processes with `shoal stop {name}`, then retry",
        issues(&reports)
    );
    Ok(())
}
