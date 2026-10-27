//! When a place a host opened for a workspace closes: once the workspace is
//! removed, or once a completion that keeps the workspace has its agent's
//! final turn finished. A place whose agent is stopped and waits for
//! `shoal resume` stays open, showing why it stopped and where it resumes.
//! A detached watcher decides this for each place, and a marker it keeps
//! tells commands run in the place that it closes with the workspace.
use std::{future::Future, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context as _, Result};

use super::{HOSTS, Host};
use crate::{
    cli::{client, context::Context, installed, internal},
    env,
    paths::Paths,
    state::AgentState,
};

/// What a host shows of a place it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum PlaceState {
    /// The host reports the place gone, or hosting something else.
    Gone,
    /// The place is open, with its agent's turn state when the host reads one.
    Open(Option<AgentState>),
}

/// Start a detached watcher that closes `place` of `host` with `workspace`.
/// It inherits the place's shell directive to remove it with the place,
/// whose shell may close before a prompt consumes it.
pub(super) fn start_watcher(
    ctx: &Context,
    host: &dyn Host,
    place: &str,
    workspace: &str,
) -> Result<()> {
    let argv = internal::internal_command(
        &ctx.paths,
        ctx.json,
        internal::Worker::PlaceWatch {
            host: host.name(),
            workspace,
            place,
        },
    )?;
    let mut watcher = tokio::process::Command::new(&argv[0]);
    watcher
        .args(&argv[1..])
        .current_dir(&ctx.paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        watcher.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    watcher
        .spawn()
        .with_context(|| format!("watch the workspace to close its {} place", host.name()))?;
    Ok(())
}

/// The watcher: close `place` of the host named `host` once `workspace`
/// says so. One refused for a daemon protocol change continues from the
/// installed binary once an upgrade replaced this one.
pub async fn watch(ctx: &Context, host: String, workspace: String, place: String) -> Result<i32> {
    let host = super::named(&host).with_context(|| format!("unknown terminal host {host}"))?;
    let _closing = Marker::record(&ctx.paths, host.name(), &workspace, &place);
    let executable = installed::Executable::current();
    let lifecycle = || async {
        match workspace_lifecycle(ctx, &workspace).await {
            Ok(lifecycle) => Ok(Some(lifecycle)),
            Err(error) if error.is::<client::ProtocolMismatch>() => {
                match executable.take_over(installed::arguments) {
                    Some(failure) => Err(failure),
                    None => Ok(None),
                }
            }
            Err(_) => Ok(None),
        }
    };
    close_when_due(host, &place, &workspace, lifecycle, || {
        ctx.paths.state.exists()
    })
    .await?;
    discard_inherited_directive();
    Ok(0)
}

/// Close `place` of `workspace` once `lifecycle` and the host say so,
/// looking again while `watching` holds. A missing daemon or a failed query
/// does not prove completion or removal, so `lifecycle` gives `None` then,
/// or the failure that ends the watch. A closed place leaves nothing to
/// close, whatever the daemon reports; a failed host query, like a failed
/// daemon query, proves neither.
async fn close_when_due<F>(
    host: &dyn Host,
    place: &str,
    workspace: &str,
    mut lifecycle: impl FnMut() -> F,
    watching: impl Fn() -> bool,
) -> Result<()>
where
    F: Future<Output = Result<Option<Lifecycle>>>,
{
    while watching() {
        let lifecycle = lifecycle().await?;
        let reported = match host.place_state(place, workspace).await {
            Ok(PlaceState::Gone) => break,
            Ok(PlaceState::Open(agent)) => Some(agent),
            Err(_) => None,
        };
        if let (Some(lifecycle), Some(reported)) = (lifecycle, reported)
            && lifecycle.closes(reported)
        {
            if let Err(error) = host.close_place(place).await {
                eprintln!("warning: {error:#}");
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// Remove the shell directive this process inherited from its place, whose
/// shell is gone or about to go without reaching a prompt.
fn discard_inherited_directive() {
    if let Some(directive) = std::env::var_os(env::SHELL_DIRECTIVE) {
        let _ = std::fs::remove_file(directive);
    }
}

/// Kept while a watcher waits to close a place with its workspace, so
/// commands run in that place can tell; the workspace's state goes with its
/// record.
struct Marker(Option<PathBuf>);

impl Marker {
    fn path(paths: &Paths, host: &str, workspace: &str, place: &str) -> Option<PathBuf> {
        (!place.is_empty() && !place.contains('/')).then(|| {
            paths
                .workspace_state(workspace)
                .join(format!("closing-{host}-{place}"))
        })
    }

    /// Best effort: without the record, commands in the place cannot tell.
    fn record(paths: &Paths, host: &str, workspace: &str, place: &str) -> Self {
        let path = Self::path(paths, host, workspace, place).filter(|path| {
            path.parent()
                .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                && std::fs::write(path, "").is_ok()
        });
        Self(path)
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Whether this process runs in a place that closes once `workspace` is
/// removed.
pub(super) fn closes_with(paths: &Paths, workspace: &str) -> bool {
    HOSTS.iter().any(|host| {
        host.this_place()
            .and_then(|place| Marker::path(paths, host.name(), workspace, &place))
            .is_some_and(|path| path.exists())
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Lifecycle {
    Active,
    /// Done without cleanup. `running` while a tracked execution has not
    /// resolved, whose agent's hooks last reported `turn`; `stopped` while an
    /// agent or command waits for `shoal resume`.
    Kept {
        running: bool,
        stopped: bool,
        turn: Option<AgentState>,
    },
    Removed,
}

impl Lifecycle {
    /// A kept completion waits for the agent's final response: its tracked
    /// execution resolves, or its turn finishes as its hooks report it, or,
    /// for an agent without them, as the host `reported` it. A stopped agent
    /// keeps the place, which shows why it stopped and where it resumes.
    fn closes(&self, reported: Option<AgentState>) -> bool {
        match self {
            Self::Active | Self::Kept { stopped: true, .. } => false,
            Self::Kept { running, turn, .. } => {
                !running || turn.or(reported) == Some(AgentState::Idle)
            }
            Self::Removed => true,
        }
    }
}

async fn workspace_lifecycle(ctx: &Context, workspace: &str) -> Result<Lifecycle> {
    match client::inspect(&ctx.paths, workspace.into()).await {
        // Completion cleanup keeps the place until removal, after its grace period.
        Ok(inspection)
            if inspection
                .completion
                .as_ref()
                .is_some_and(|completion| !completion.cleanup) =>
        {
            Ok(Lifecycle::Kept {
                running: !inspection.executions.is_empty(),
                stopped: crate::execution::recovery::pending(&ctx.paths, workspace)?,
                turn: inspection.workspace.agent_state.map(|status| status.state),
            })
        }
        Ok(_) => Ok(Lifecycle::Active),
        Err(error) => {
            if client::workspaces(&ctx.paths)
                .await?
                .iter()
                .any(|item| item.id == workspace)
            {
                return Err(error);
            }
            Ok(Lifecycle::Removed)
        }
    }
}

#[cfg(test)]
mod tests;
