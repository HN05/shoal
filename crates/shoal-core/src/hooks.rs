//! Untracked user hooks receive the identity of their space, a workspace or
//! chat, without an execution scope.
use std::{collections::BTreeMap, ffi::OsString, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub use crate::config::hooks::{HookDirectory, HookKind};
use crate::{
    env,
    model::{Part, Space},
    paths::Paths,
    protocol::timing,
};
use shoal_protocol::resources::ResourceLease;

/// Give `command` the identity of `space`, whose hook it runs: its own,
/// and the path of the worktree it owns, which a chat has none of.
fn apply_identity<P: Part>(command: &mut Command, space: &Space<P>, paths: &Paths) {
    env::apply_space_identity(command, &space.into(), paths);
    match space.worktree() {
        Some(worktree) => command.env(env::WORKSPACE_PATH, &worktree.path),
        None => command.env_remove(env::WORKSPACE_PATH),
    };
}

/// Longest a daemon-side hook may run before the operation fails.
const DETACHED_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub enum Hook<'a> {
    PreSetup,
    PostSetup,
    PreRemove,
    PostRemove(&'a Path),
    PostDone(&'a crate::model::Completion),
    PostReady(&'a [crate::model::ReviewMark]),
    PostAgentExit {
        agent: &'a crate::agent::Agent,
        exit_code: Option<i32>,
        complete: bool,
    },
    PostResourceAcquire(&'a ResourceLease),
    PreResourceRelease(&'a ResourceLease),
}

impl Hook<'_> {
    pub(crate) fn kind(self) -> HookKind {
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

fn command<P: Part>(
    hook: Hook<'_>,
    space: &Space<P>,
    executable: &Path,
    paths: &Paths,
    configured_env: &BTreeMap<String, OsString>,
) -> Result<Command> {
    let mut command = Command::new(executable);
    apply_identity(&mut command, space, paths);
    for name in env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = env::path_without_secret_tools() {
        command.env("PATH", path);
    }
    command
        .current_dir(hook.kind().directory().path(
            space.workdir.path(),
            match hook {
                Hook::PostRemove(checkout) => checkout,
                _ => space.workdir.path(),
            },
        ))
        .env(env::HOOK, hook.kind().as_str())
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
    workspace: &crate::model::Workspace,
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
            stop_group(&mut child, group).await?;
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

/// Stop the hook's process `group`, which `command` started it in: SIGTERM,
/// then SIGKILL unless the whole group exits within the stop grace, as a
/// tracked command is stopped.
async fn stop_group(child: &mut tokio::process::Child, group: i32) -> Result<()> {
    // SAFETY: signalling the hook's own process group.
    unsafe {
        libc::kill(-group, libc::SIGTERM);
    }
    let exited = async {
        child.wait().await?;
        crate::process::identity::group_exited(group as u32).await
    };
    if !matches!(
        tokio::time::timeout(timing::EXECUTION_STOP_GRACE, exited).await,
        Ok(Ok(()))
    ) {
        // SAFETY: as above.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        child.wait().await?;
    }
    Ok(())
}

/// Run a hook from the daemon, without a terminal and with a time limit.
pub async fn run_detached<P: Part>(
    hook: Hook<'_>,
    space: &Space<P>,
    executable: &Path,
    paths: &Paths,
    configured_env: &BTreeMap<String, OsString>,
) -> Result<()> {
    let command = command(hook, space, executable, paths, configured_env)?;
    crate::subprocess::Run::new(command)
        .timeout(DETACHED_TIMEOUT)
        .checked()
        .await
        .with_context(|| format!("run {} {}", hook.kind().key(), executable.display()))?;
    Ok(())
}
