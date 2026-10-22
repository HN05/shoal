//! Explicit, untracked desktop-app handoffs.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::trust::TrustFile;
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    env, execution,
    model::Workspace,
};

/// `shoal t3` and other shortcuts that only open an app.
pub(in crate::cli) async fn shortcut(
    ctx: &Context,
    workspace: Option<String>,
    program: &str,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    open_app(ctx, &workspace, program, None, args).await
}

/// Desktop launchers hand the directory to another process. Their short-lived
/// command is not the agent session and must not own/kill the app's process
/// group. `trust` is the app's trust file, when it has one.
pub(super) async fn open_app(
    ctx: &Context,
    workspace: &Workspace,
    program: &str,
    trust: Option<&TrustFile>,
    args: Vec<OsString>,
) -> Result<i32> {
    super::ensure_program(&format!("{program} app"), program)?;
    ensure!(workspace.path.is_dir(), "workspace directory is missing");
    if let Some(trust) = trust {
        trust.trust_workspace(ctx, workspace).await;
    }
    let status = tokio::process::Command::new(program)
        .arg("app")
        .arg(&workspace.path)
        .args(args)
        .current_dir(&workspace.path)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| format!("launch {program} app"))?;
    Ok(execution::exit_code(status))
}
