//! When a place a host opened for a workspace closes: once the workspace is
//! removed, or once a completion that keeps the workspace has its agent's
//! final turn finished. A place whose agent is stopped and waits for
//! `shoal resume` stays open, showing why it stopped and where it resumes.
//! A detached watcher decides this for each place, and a marker it keeps
//! tells commands run in the place that it closes with the workspace.
use std::{future::Future, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context as _, Result};

use super::{Host, host::Watches, hosts};
use crate::{
    cli::{client, context::Context, installed, internal},
    env,
    model::Inspection,
    paths::Paths,
    sessions::PlaceClose,
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

/// A place a watcher closes: the workspace it hosts, and what told it apart
/// from a later place under the same ID when the watch began.
pub(in crate::cli) struct Watched {
    pub id: String,
    pub workspace: String,
    pub instance: Option<String>,
}

/// Start a detached watcher that closes `place` of `host` with `workspace`.
/// It records which instance of the place it watches and the kept
/// completion, if any, that the agent starts under, so a restart of the
/// watcher keeps both. It inherits the place's shell directive to remove it
/// with the place, whose shell may close before a prompt consumes it.
pub(super) async fn start_watcher(
    ctx: &Context,
    host: &dyn Host,
    place: &str,
    workspace: &str,
) -> Result<()> {
    let instance = match host.watches() {
        Some(watches) => watches.place_instance(place).await?,
        None => None,
    };
    let kept_at = kept_completion(ctx, workspace).await?;
    let argv = internal::internal_command(
        &ctx.paths,
        ctx.json,
        internal::Worker::PlaceWatch {
            host: host.name().as_str(),
            workspace,
            place,
            instance: instance.as_deref(),
            kept_at: kept_at.as_deref(),
        },
    )?;
    spawn_detached(ctx, &argv)
        .with_context(|| format!("watch the workspace to close its {} place", host.name()))
}

/// Start `argv` in its own session, apart from this process and its terminal.
fn spawn_detached(ctx: &Context, argv: &[std::ffi::OsString]) -> std::io::Result<()> {
    let mut child = tokio::process::Command::new(&argv[0]);
    child
        .args(&argv[1..])
        .current_dir(&ctx.paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    child.spawn()?;
    Ok(())
}

/// The HEAD of `workspace`'s kept completion, if it has one.
async fn kept_completion(ctx: &Context, workspace: &str) -> Result<Option<String>> {
    let inspection = client::inspect(&ctx.paths, workspace.into()).await?;
    Ok(inspection
        .completion
        .filter(|completion| !completion.cleanup)
        .map(|completion| completion.head))
}

/// The watcher: close `place` of the host named `host` once its workspace
/// says so, ignoring the completion at `kept_at` it began under. One
/// refused for a daemon protocol change continues from the installed binary
/// once an upgrade replaced this one.
pub async fn watch(
    ctx: &Context,
    host: String,
    place: Watched,
    kept_at: Option<String>,
) -> Result<i32> {
    let host = super::named(&host).with_context(|| format!("unknown terminal host {host}"))?;
    let watches = host
        .watches()
        .with_context(|| format!("{} cannot read places", host.name()))?;
    let _closing = Marker::record(
        &ctx.paths,
        host.name().as_str(),
        &place.workspace,
        &place.id,
    );
    let executable = installed::Executable::current();
    let lifecycle = || async {
        match workspace_lifecycle(ctx, &place.workspace).await {
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
    let earlier = EarlierCompletion(kept_at);
    let close = async || close_place(ctx, host, &place).await;
    close_when_due(watches, &place, earlier, lifecycle, close, || {
        ctx.paths.state.exists()
    })
    .await?;
    discard_inherited_directive();
    Ok(0)
}

/// Close `place` through the daemon while it records the place as its
/// workspace's, otherwise through its host. A place another workspace's
/// agent took over since stays open.
async fn close_place(ctx: &Context, host: &dyn Host, place: &Watched) -> Result<()> {
    let closed =
        client::close_owned_place(&ctx.paths, &place.workspace, host.name(), &place.id).await?;
    close_left_to_host(host, place, closed).await
}

/// Close `place` through its host when the daemon, which `closed` says
/// what it did, leaves that to the host: it cannot close the host's places,
/// or it keeps no record of the place. Only a host whose places prove their
/// own ownership closes them.
async fn close_left_to_host(host: &dyn Host, place: &Watched, closed: PlaceClose) -> Result<()> {
    match closed {
        PlaceClose::HostCloses | PlaceClose::NotRecorded => match host.closes() {
            Some(closes) => closes.close_place(&place.id).await,
            None => Ok(()),
        },
        PlaceClose::Closed | PlaceClose::OwnedElsewhere => Ok(()),
    }
}

/// Close `place` with `close` once `lifecycle` and the host say so,
/// looking again while `watching` holds. A missing daemon or a failed query
/// does not prove completion or removal, so `lifecycle` gives `None` then,
/// or the failure that ends the watch. A closed place leaves nothing to
/// close, whatever the daemon reports; a failed host query, like a failed
/// daemon query, proves neither, and neither does the `earlier` completion.
async fn close_when_due<F>(
    host: &dyn Watches,
    place: &Watched,
    mut earlier: EarlierCompletion,
    mut lifecycle: impl FnMut() -> F,
    close: impl AsyncFn() -> Result<()>,
    watching: impl Fn() -> bool,
) -> Result<()>
where
    F: Future<Output = Result<Option<Lifecycle>>>,
{
    while watching() {
        let lifecycle = lifecycle()
            .await?
            .filter(|lifecycle| !earlier.only_one_in(lifecycle));
        let reported = match host.place_state(place).await {
            Ok(PlaceState::Gone) => break,
            Ok(PlaceState::Open(agent)) => Some(agent),
            Err(_) => None,
        };
        if let (Some(lifecycle), Some(reported)) = (lifecycle, reported)
            && lifecycle.closes(reported)
        {
            if let Err(error) = close().await {
                eprintln!("warning: {error:#}");
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// The kept completion a watch began under, by its HEAD: an agent started in
/// a workspace already done is new work, which that completion does not
/// end. Withdrawing it, or completing again at another HEAD, ends the
/// exemption.
struct EarlierCompletion(Option<String>);

impl EarlierCompletion {
    /// Whether `lifecycle` shows only the completion the watch began under.
    fn only_one_in(&mut self, lifecycle: &Lifecycle) -> bool {
        let head = match lifecycle {
            Lifecycle::Kept { head, .. } => Some(head.as_str()),
            Lifecycle::Active | Lifecycle::Removed => None,
        };
        if self.0.as_deref() != head {
            self.0 = None;
        }
        self.0.is_some()
    }
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
        Self::named(paths, workspace, place, &format!("closing-{host}-{place}"))
    }

    /// The marker a Herdr tab watcher of an earlier release keeps until an
    /// upgrade hands it to this one.
    fn earlier_path(paths: &Paths, workspace: &str, place: &str) -> Option<PathBuf> {
        Self::named(paths, workspace, place, &format!("closing-tab-{place}"))
    }

    fn named(paths: &Paths, workspace: &str, place: &str, name: &str) -> Option<PathBuf> {
        (!place.is_empty() && !place.contains('/'))
            .then(|| paths.workspace_state(workspace).join(name))
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
    hosts().any(|host| {
        super::this_place(host).is_some_and(|place| {
            [
                Marker::path(paths, host.name().as_str(), workspace, &place),
                Marker::earlier_path(paths, workspace, &place),
            ]
            .into_iter()
            .flatten()
            .any(|path| path.exists())
        })
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Lifecycle {
    Active,
    /// Done without cleanup at `head`. `running` while a tracked execution
    /// has not resolved, whose agent's hooks last reported `turn`; `stopped`
    /// while an agent or command waits for `shoal resume`.
    Kept {
        head: String,
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
        Ok(Inspection {
            completion: Some(completion),
            executions,
            workspace: record,
            ..
        }) if !completion.cleanup => Ok(Lifecycle::Kept {
            head: completion.head,
            running: !executions.is_empty(),
            stopped: crate::execution::recovery::pending(&ctx.paths, workspace)?,
            turn: record.agent_state.map(|status| status.state),
        }),
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
