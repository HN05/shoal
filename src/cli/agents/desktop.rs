//! Explicit, untracked desktop-app handoffs.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::trust::trust_codex;
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    env, execution,
};

/// Desktop launchers hand the directory to another process. Their short-lived
/// command is not the agent session and must not own/kill the app's process group.
pub(in crate::cli) async fn open_app(
    ctx: &Context,
    workspace: Option<String>,
    program: &str,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    if program == "codex" {
        trust_codex(ctx, &inspection.workspace.path);
    }
    let status = tokio::process::Command::new(program)
        .arg("app")
        .arg(&inspection.workspace.path)
        .args(args)
        .current_dir(&inspection.workspace.path)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| {
            format!("launch {program} app; install {program} and make it available on PATH")
        })?;
    Ok(execution::exit_code(status))
}
