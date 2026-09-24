//! Untracked user hooks receive workspace identity without an execution scope.
use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::{
    config::{Config, repo::RepoConfig},
    daemon::resources::ResourceLease,
    env,
    model::Workspace,
    paths::Paths,
};

/// Longest a daemon-side hook may run before the operation fails.
const DETACHED_TIMEOUT: Duration = Duration::from_secs(60);

/// The directory used both to resolve a hook's executable and to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDirectory {
    Worktree,
    Checkout,
}

impl HookDirectory {
    pub fn path<'a>(self, worktree: &'a Path, checkout: &'a Path) -> &'a Path {
        match self {
            Self::Worktree => worktree,
            Self::Checkout => checkout,
        }
    }
}

// Keep the config field, environment name, global eligibility and directory in
// one table. Config structs retain their existing TOML keys and strict parsing.
macro_rules! hook_kinds {
    ($($kind:ident => ($field:ident, $name:literal, $global:tt, $directory:ident)),+ $(,)?) => {
        crate::state::states!(HookKind { $($kind => $name),+ });

        impl HookKind {
            pub const ALL: &[Self] = &[$(Self::$kind),+];

            pub fn key(self) -> &'static str {
                match self { $(Self::$kind => stringify!($field)),+ }
            }

            pub fn allows_global(self) -> bool {
                match self { $(Self::$kind => $global),+ }
            }

            pub fn directory(self) -> HookDirectory {
                match self { $(Self::$kind => HookDirectory::$directory),+ }
            }

            pub fn repository_command(self, config: &RepoConfig) -> Option<&String> {
                match self { $(Self::$kind => config.$field.as_ref()),+ }
            }

            pub fn repository_command_mut(self, config: &mut RepoConfig) -> &mut Option<String> {
                match self { $(Self::$kind => &mut config.$field),+ }
            }

            pub fn global_command(self, config: &Config) -> Option<&String> {
                match self { $(Self::$kind => hook_kinds!(@global config, $field, $global)),+ }
            }

            pub fn validate(self, command: Option<&String>) -> Result<()> {
                if let Some(command) = command {
                    ensure!(
                        !command.trim().is_empty() && !command.contains('\0'),
                        "{} must be a nonempty executable path", self.key()
                    );
                }
                Ok(())
            }
        }
    };
    (@global $config:ident, $field:ident, true) => { $config.$field.as_ref() };
    (@global $config:ident, $field:ident, false) => { None };
}

hook_kinds! {
    Setup => (setup_cmd, "setup", false, Worktree),
    PreSetup => (pre_setup_cmd, "pre_setup", true, Worktree),
    PostSetup => (post_setup_cmd, "post_setup", false, Worktree),
    PreRemove => (pre_remove_cmd, "pre_remove", false, Worktree),
    PostRemove => (post_remove_cmd, "post_remove", true, Checkout),
    PostResourceAcquire => (post_resource_acquire_cmd, "post_resource_acquire", true, Worktree),
    PreResourceRelease => (pre_resource_release_cmd, "pre_resource_release", true, Worktree),
}

#[derive(Debug, Clone, Copy)]
pub enum Hook<'a> {
    PreSetup,
    PostSetup,
    PreRemove,
    PostRemove(&'a Path),
    PostResourceAcquire(&'a ResourceLease),
    PreResourceRelease(&'a ResourceLease),
}

impl Hook<'_> {
    pub fn kind(self) -> HookKind {
        match self {
            Hook::PreSetup => HookKind::PreSetup,
            Hook::PostSetup => HookKind::PostSetup,
            Hook::PreRemove => HookKind::PreRemove,
            Hook::PostRemove(_) => HookKind::PostRemove,
            Hook::PostResourceAcquire(_) => HookKind::PostResourceAcquire,
            Hook::PreResourceRelease(_) => HookKind::PreResourceRelease,
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
    env::apply_workspace_identity(&mut command, workspace, paths);
    command
        .current_dir(hook.kind().directory().path(
            &workspace.path,
            match hook {
                Hook::PostRemove(checkout) => checkout,
                _ => &workspace.path,
            },
        ))
        .env(env::HOOK, hook.kind().as_str())
        .env(env::WORKSPACE_PATH, &workspace.path)
        .env_remove(env::RESOURCE_LEASE)
        .env_remove(env::SCOPE_TOKEN)
        .env_remove(env::EXECUTION_ID)
        .env_remove(env::RESERVED_PORT_ENV)
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
        .with_context(|| format!("launch {} {}", hook.kind().key(), executable.display()))?;
    let group = child.id().context("hook process ID unavailable")? as i32;
    let _terminal = crate::execution::Terminal::give_to_if_foreground(group)?;
    // The hook may have stopped on terminal I/O before becoming foreground.
    unsafe {
        libc::kill(-group, libc::SIGCONT);
    }
    let status = child
        .wait()
        .await
        .with_context(|| format!("wait for {} {}", hook.kind().key(), executable.display()))?;
    ensure!(
        status.success(),
        "{} exited with {}; the workspace is kept",
        hook.kind().key(),
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
    let output = crate::subprocess::Run::new(command(hook, workspace, executable, paths)?)
        .timeout(DETACHED_TIMEOUT)
        .capture()
        .await
        .with_context(|| format!("run {} {}", hook.kind().key(), executable.display()))?;
    let diagnostic = crate::subprocess::diagnostic(&output.stderr);
    ensure!(
        output.status.success(),
        "{} exited with {}: {}",
        hook.kind().key(),
        crate::execution::exit_code(output.status),
        diagnostic.trim()
    );
    Ok(())
}
