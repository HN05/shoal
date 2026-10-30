//! Untracked user hooks receive workspace identity without an execution scope.
use std::{collections::BTreeMap, ffi::OsString, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::{
    config::{Settings, repo::RepoConfig},
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

// Keep the config field, environment name and directory in one table. Config
// structs retain their existing TOML keys and strict parsing.
macro_rules! hook_kinds {
    ($($kind:ident => ($($field:ident).+, $name:literal, $directory:ident)),+ $(,)?) => {
        crate::state::states!(HookKind { $($kind => $name),+ });

        impl HookKind {
            pub const ALL: &[Self] = &[$(Self::$kind),+];

            pub fn key(self) -> &'static str {
                match self { $(Self::$kind => stringify!($($field).+)),+ }
            }

            pub fn directory(self) -> HookDirectory {
                match self { $(Self::$kind => HookDirectory::$directory),+ }
            }

            pub fn repository_command(self, config: &RepoConfig) -> Option<&String> {
                match self { $(Self::$kind => config.$($field).+.as_ref()),+ }
            }

            pub fn repository_command_mut(self, config: &mut RepoConfig) -> Option<&mut String> {
                match self { $(Self::$kind => config.$($field).+.as_mut()),+ }
            }

            pub fn command(self, settings: &Settings) -> Option<&String> {
                match self { $(Self::$kind => settings.$($field).+.as_ref()),+ }
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
}

hook_kinds! {
    Setup => (setup.command, "setup", Worktree),
    PreSetup => (hooks.pre_setup, "pre_setup", Worktree),
    PostSetup => (hooks.post_setup, "post_setup", Worktree),
    PreRemove => (hooks.pre_remove, "pre_remove", Worktree),
    PostRemove => (hooks.post_remove, "post_remove", Checkout),
    PostDone => (hooks.post_done, "post_done", Worktree),
    PostReady => (hooks.post_ready, "post_ready", Worktree),
    PostAgentExit => (hooks.post_agent_exit, "post_agent_exit", Worktree),
    PostResourceAcquire => (hooks.post_resource_acquire, "post_resource_acquire", Worktree),
    PreResourceRelease => (hooks.pre_resource_release, "pre_resource_release", Worktree),
}

#[derive(Debug, Clone, Copy)]
pub enum Hook<'a> {
    PreSetup,
    PostSetup,
    PreRemove,
    PostRemove(&'a Path),
    PostDone(&'a crate::model::Completion),
    PostReady(&'a [crate::model::ReviewMark]),
    PostAgentExit {
        agent: &'a str,
        exit_code: Option<i32>,
        complete: bool,
    },
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
            Hook::PostDone(_) => HookKind::PostDone,
            Hook::PostReady(_) => HookKind::PostReady,
            Hook::PostAgentExit { .. } => HookKind::PostAgentExit,
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
    configured_env: &BTreeMap<String, OsString>,
) -> Result<Command> {
    let mut command = Command::new(executable);
    env::apply_workspace_identity(&mut command, workspace, paths);
    for name in env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = env::path_without_secret_tools() {
        command.env("PATH", path);
    }
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
        .env_remove(env::AGENT)
        .env_remove(env::AGENT_EXIT_CODE)
        .env_remove(env::AGENT_EXIT_COMPLETE)
        .env_remove(env::DONE_CHOICE)
        .env_remove(env::REVIEW_MARKS)
        .env_remove(env::RESOURCE_LEASE)
        .env_remove(env::SCOPE_TOKEN)
        .env_remove(env::OUTPUT)
        .env_remove(env::EXECUTION_ID)
        .env_remove(env::RESERVED_PORT_ENV)
        .env_remove(env::SECRET_ENV)
        .env_remove(env::SECRET_TOOLS)
        .process_group(0)
        .kill_on_drop(true);
    env::apply_configured(&mut command, configured_env);
    if let Hook::PostResourceAcquire(lease) | Hook::PreResourceRelease(lease) = hook {
        command.env(env::RESOURCE_LEASE, serde_json::to_string(lease)?);
    }
    if let Hook::PostReady(marks) = hook {
        command.env(env::REVIEW_MARKS, serde_json::to_string(marks)?);
    }
    if let Hook::PostDone(completion) = hook {
        command.env(
            env::DONE_CHOICE,
            if completion.cleanup {
                "cleanup"
            } else {
                "keep"
            },
        );
    }
    if let Hook::PostAgentExit {
        agent,
        exit_code,
        complete,
    } = hook
    {
        command
            .env(env::AGENT, agent)
            .env(
                env::AGENT_EXIT_CODE,
                exit_code.map(|code| code.to_string()).unwrap_or_default(),
            )
            .env(
                env::AGENT_EXIT_COMPLETE,
                if complete { "true" } else { "false" },
            );
    }
    Ok(command)
}

/// Run a hook without a terminal, its output on this process's, until it
/// exits or `stop` completes, which stops its process group.
pub async fn run_until(
    hook: Hook<'_>,
    workspace: &Workspace,
    executable: &Path,
    paths: &Paths,
    configured_env: &BTreeMap<String, OsString>,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    let mut child = command(hook, workspace, executable, paths, configured_env)?
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("launch {} {}", hook.kind().key(), executable.display()))?;
    let group = child.id().context("hook process ID unavailable")? as i32;
    let status = tokio::select! {
        status = child.wait() => status,
        () = stop => {
            // SAFETY: signalling the hook's own process group, which
            // `command` started it in.
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
            child.wait().await?;
            anyhow::bail!("{} was stopped; the workspace is kept", hook.kind().key());
        }
    }
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
    configured_env: &BTreeMap<String, OsString>,
) -> Result<()> {
    let output =
        crate::subprocess::Run::new(command(hook, workspace, executable, paths, configured_env)?)
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
