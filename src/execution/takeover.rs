//! An agent's wrapper whose binary an upgrade replaced continues from the
//! installed one when it restores the agent, as `shoal restart` does: it
//! replaces itself with that binary, keeping its process, terminal and daemon
//! connection, and the installed release registers again and starts the
//! agent. The daemon sends nothing between its resume and the restored
//! child's start, so the handover loses no control.
use std::{
    os::{
        fd::{FromRawFd as _, RawFd},
        unix::process::CommandExt as _,
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use tokio::io::BufReader;

use super::{Mode, link::Link};
use crate::{
    cli::internal::{Worker, internal_command},
    env,
    fsutil::set_inherited,
    model::ExecutionPlan,
    paths::Paths,
    protocol::{Control, ExecutionEvent, timing},
    sessions::Origin,
};

/// The binary the wrapper continues from: the installed one, once an
/// upgrade put it in place of the wrapper's own, while `link` can hand its
/// connection over to a `daemon` that runs the installed binary too.
pub(super) fn installed(link: &Link, daemon: bool) -> Option<PathBuf> {
    if !daemon {
        return None;
    }
    link.descriptor()?;
    crate::cli::installed::started()
        .installed()
        .map(Path::to_owned)
}

/// How long the installed binary may prepare the agent it restores: its
/// secret and key commands run while the daemon awaits the agent's start
/// without taking stops.
pub(super) const PREPARATION_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The agent a wrapper hands over, which the binary it continues from
/// restores.
pub(super) struct Handover<'a> {
    pub workspace: &'a str,
    pub agent: &'a str,
    /// The recovery record the agent resumes from.
    pub record: Option<&'a Path>,
}

/// Replace this wrapper with `installed`, handing it `link` and the agent.
/// Returns only on failure, after which the wrapper finishes.
pub(super) fn exec(
    paths: &Paths,
    installed: &Path,
    link: &Link,
    mode: &Mode,
    handover: Handover<'_>,
) -> anyhow::Error {
    let Some(connection) = link.descriptor() else {
        return anyhow::anyhow!("the daemon connection is not attached");
    };
    match continue_with(paths, installed, connection, mode, handover) {
        Ok(error) => anyhow::Error::new(error),
        Err(error) => error,
    }
    .context("continue with the installed shoal")
}

/// Exec `installed` as the take-over worker over descriptor `connection`;
/// the error exec returned, or the failure before it.
fn continue_with(
    paths: &Paths,
    installed: &Path,
    connection: RawFd,
    mode: &Mode,
    handover: Handover<'_>,
) -> Result<std::io::Error> {
    let log = match mode {
        Mode::Detached { log } => Some(log.as_path()),
        _ => None,
    };
    let argv = internal_command(
        paths,
        false,
        Worker::TakeOver {
            workspace: handover.workspace,
            agent: handover.agent,
            connection,
            record: handover.record,
            log,
        },
    )?;
    let mut command = std::process::Command::new(installed);
    command.args(&argv[1..]);
    // This process took the marker out of its environment as it started.
    if env::ends_launch_session() {
        command.env(env::ENDS_SESSION, "1");
    }
    set_inherited(connection, true)?;
    // The installed binary writes to the same output after this.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    Ok(command.exec())
}

/// The daemon connection a wrapper handed over at descriptor `fd`, closed to
/// the programs this process starts.
pub(super) fn connection(fd: RawFd) -> Result<Link> {
    ensure!(
        fd > 2 && is_socket(fd),
        "descriptor {fd} is not a daemon connection"
    );
    set_inherited(fd, false)?;
    // SAFETY: the wrapper this process continues handed it the open
    // descriptor, which nothing else in this process owns.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;
    let (reader, writer) = tokio::net::UnixStream::from_std(stream)
        .context("adopt the daemon connection")?
        .into_split();
    Ok(Link::new(BufReader::new(reader), writer))
}

fn is_socket(fd: RawFd) -> bool {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat fills `status` when it succeeds, and only then is it read.
    unsafe {
        libc::fstat(fd, status.as_mut_ptr()) == 0
            && status.assume_init().st_mode & libc::S_IFMT == libc::S_IFSOCK
    }
}

/// Register the execution again over `link` from `place`, where this
/// process runs, returning its plan.
pub(super) async fn register_again(
    link: &mut Link,
    place: Option<Origin>,
) -> Result<ExecutionPlan> {
    link.send(&ExecutionEvent::Register {
        origin: place.map(Box::new),
    })
    .await?;
    let answer = tokio::time::timeout(timing::EXECUTION_START_TIMEOUT, link.control())
        .await
        .context("the daemon did not answer the registration")??;
    match answer {
        Control::Registered { plan } => Ok(*plan),
        Control::Stop {
            reason: Some(reason),
            ..
        } => bail!("the daemon refused the agent: {reason}"),
        control => bail!("unexpected answer to the registration: {control:?}"),
    }
}
