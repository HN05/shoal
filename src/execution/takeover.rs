//! An agent's wrapper whose binary an upgrade replaced continues from the
//! installed one when it restores the agent, as `shoal restart` does: it
//! replaces itself with that binary, keeping its process, terminal and daemon
//! connection, and the installed release registers again and starts the
//! agent. The daemon sends nothing between its resume and the restored
//! child's start, so the handover loses no control.
use std::os::fd::{FromRawFd as _, RawFd};

use anyhow::{Context, Result, bail, ensure};
use tokio::io::BufReader;

use super::link::Link;
use crate::{
    fsutil::set_inherited,
    model::ExecutionPlan,
    protocol::{Control, ExecutionEvent, timing},
    sessions::Origin,
};

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
