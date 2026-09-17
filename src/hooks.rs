//! Repository lifecycle hooks: `post_setup_cmd` after a workspace is ready and
//! `pre_remove_cmd` before its worktree is removed. Hooks are the user's own
//! untracked processes, so a tmux server or other daemon they leave behind is
//! not an execution survivor. They receive the workspace identity, not a scope
//! token.
use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::{process::Command, time::timeout};

use crate::{env, model::Workspace, paths::Paths};

/// Longest a daemon-side hook may run before removal fails.
const DETACHED_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DIAGNOSTIC_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub enum Hook {
    PostSetup,
    PreRemove,
}

impl Hook {
    fn key(self) -> &'static str {
        match self {
            Hook::PostSetup => "post_setup_cmd",
            Hook::PreRemove => "pre_remove_cmd",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Hook::PostSetup => "post_setup",
            Hook::PreRemove => "pre_remove",
        }
    }
}

fn command(hook: Hook, workspace: &Workspace, executable: &Path, paths: &Paths) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(&workspace.path)
        .env(env::HOOK, hook.name())
        .env(env::WORKSPACE_ID, &workspace.id)
        .env(env::RUN_ID, &workspace.id)
        .env(env::WORKSPACE_NAME, &workspace.name)
        .env(env::STATE_DIR, &paths.state)
        .env_remove(env::SHELL_DIRECTIVE)
        .kill_on_drop(true);
    command
}

/// Run a hook from the CLI with the caller's terminal. `quiet` keeps stdout
/// clean for `--json` output, as setup does.
pub async fn run_interactive(
    hook: Hook,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
    quiet: bool,
) -> Result<()> {
    let status = command(hook, workspace, executable, paths)
        .stdin(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(if quiet {
            Stdio::from(std::io::stderr())
        } else {
            Stdio::inherit()
        })
        .status()
        .await
        .with_context(|| format!("launch {} {}", hook.key(), executable.display()))?;
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
    hook: Hook,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
) -> Result<()> {
    let output = timeout(
        DETACHED_TIMEOUT,
        command(hook, workspace, executable, paths)
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
