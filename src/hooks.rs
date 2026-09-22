//! Untracked user hooks receive workspace identity without an execution scope.
use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::{process::Command, time::timeout};

use crate::{env, model::Workspace, paths::Paths, resources::ResourceLease};

/// Longest a daemon-side hook may run before the operation fails.
const DETACHED_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DIAGNOSTIC_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub enum Hook<'a> {
    PreSetup,
    PostSetup,
    PreRemove,
    PostResourceAcquire(&'a ResourceLease),
    PreResourceRelease(&'a ResourceLease),
}

impl Hook<'_> {
    fn key(self) -> &'static str {
        match self {
            Hook::PreSetup => "pre_setup_cmd",
            Hook::PostSetup => "post_setup_cmd",
            Hook::PreRemove => "pre_remove_cmd",
            Hook::PostResourceAcquire(_) => "post_resource_acquire_cmd",
            Hook::PreResourceRelease(_) => "pre_resource_release_cmd",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Hook::PreSetup => "pre_setup",
            Hook::PostSetup => "post_setup",
            Hook::PreRemove => "pre_remove",
            Hook::PostResourceAcquire(_) => "post_resource_acquire",
            Hook::PreResourceRelease(_) => "pre_resource_release",
        }
    }
}

fn command(
    hook: Hook<'_>,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
) -> Result<Command> {
    let mut command = Command::new(executable);
    for name in env::inherited_port_exports() {
        command.env_remove(name);
    }
    command
        .current_dir(&workspace.path)
        .env(env::HOOK, hook.name())
        .env(env::WORKSPACE_ID, &workspace.id)
        .env(env::WORKSPACE_PATH, &workspace.path)
        .env_remove(env::RESOURCE_LEASE)
        .env(env::RUN_ID, &workspace.id)
        .env(env::WORKSPACE_NAME, &workspace.name)
        .env(env::STATE_DIR, &paths.state)
        .env_remove(env::SCOPE_TOKEN)
        .env_remove(env::EXECUTION_ID)
        .env_remove(env::RESERVED_PORT_ENV)
        .env_remove(env::SHELL_DIRECTIVE)
        .process_group(0)
        .kill_on_drop(true);
    if let Hook::PostResourceAcquire(lease) | Hook::PreResourceRelease(lease) = hook {
        command.env(env::RESOURCE_LEASE, serde_json::to_string(lease)?);
    }
    Ok(command)
}

/// Run a hook from the CLI with the caller's terminal. `quiet` keeps stdout
/// clean for `--json` output, as setup does.
pub async fn run_interactive(
    hook: Hook<'_>,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
    quiet: bool,
) -> Result<()> {
    let background_terminal = crate::execution::Terminal::stdin_is_background();
    let mut child = command(hook, workspace, executable, paths)?
        .stdin(if quiet || background_terminal {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(if quiet {
            Stdio::from(std::io::stderr())
        } else {
            Stdio::inherit()
        })
        .spawn()
        .with_context(|| format!("launch {} {}", hook.key(), executable.display()))?;
    let group = child.id().context("hook process ID unavailable")? as i32;
    let _terminal = crate::execution::Terminal::give_to_if_foreground(group)?;
    // The hook may have stopped on terminal I/O before becoming foreground.
    unsafe {
        libc::kill(-group, libc::SIGCONT);
    }
    let status = child
        .wait()
        .await
        .with_context(|| format!("wait for {} {}", hook.key(), executable.display()))?;
    ensure!(
        status.success(),
        "{} exited with {}; the workspace is kept",
        hook.key(),
        crate::execution::exit_code(status)
    );
    Ok(())
}

/// Run a hook from the daemon, without a terminal and with a time limit.
pub async fn run_detached(
    hook: Hook<'_>,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
) -> Result<()> {
    let output = timeout(
        DETACHED_TIMEOUT,
        command(hook, workspace, executable, paths)?
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .with_context(|| {
        format!(
            "{} did not finish within {} seconds",
            hook.key(),
            DETACHED_TIMEOUT.as_secs()
        )
    })?
    .with_context(|| format!("launch {} {}", hook.key(), executable.display()))?;
    let diagnostic: String = String::from_utf8_lossy(&output.stderr)
        .chars()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect();
    ensure!(
        output.status.success(),
        "{} exited with {}: {}",
        hook.key(),
        crate::execution::exit_code(output.status),
        diagnostic.trim()
    );
    Ok(())
}
