//! Accepting client connections and the descriptors they use.

use std::{io, time::Duration};

use tokio::net::{UnixListener, UnixStream};

use super::log;

/// Open files the daemon asks for: room for thousands of connections, within
/// the per-process limit macOS accepts (`OPEN_MAX`).
const DESCRIPTOR_TARGET: u64 = 10_240;

/// Raise the soft open-file limit toward the hard limit, since launchd starts
/// services with 256, and return the limit in effect.
pub(super) fn raise_descriptor_limit() -> u64 {
    let Some(limit) = descriptor_limit() else {
        return DESCRIPTOR_TARGET;
    };
    let target = limit.rlim_max.min(DESCRIPTOR_TARGET);
    if limit.rlim_cur >= target {
        return limit.rlim_cur;
    }
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: `raised` is a valid rlimit for the duration of the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
        return target;
    }
    let error = io::Error::last_os_error();
    log!(
        "raise open file limit from {} to {target}: {error}",
        limit.rlim_cur
    );
    limit.rlim_cur
}

fn descriptor_limit() -> Option<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable rlimit for the duration of the call.
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0).then_some(limit)
}

/// Wait before accepting again after a failure that clears on its own.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The next client connection. Running out of descriptors or a client that
/// hung up before acceptance passes, so those failures wait and retry instead
/// of stopping the daemon.
pub(super) async fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if transient(&error) => {
                log!("accept connection: {error}; retrying");
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_limit_reaches_the_target_or_stays_higher() {
        let hard = descriptor_limit().unwrap().rlim_max;
        let limit = raise_descriptor_limit();
        assert!(limit >= hard.min(DESCRIPTOR_TARGET), "{limit}");
        assert_eq!(descriptor_limit().unwrap().rlim_cur, limit);
    }

    #[test]
    fn descriptor_exhaustion_and_aborted_clients_are_transient() {
        for code in [libc::EMFILE, libc::ENFILE, libc::ECONNABORTED, libc::EINTR] {
            assert!(transient(&io::Error::from_raw_os_error(code)), "{code}");
        }
        for code in [libc::EBADF, libc::EINVAL] {
            assert!(!transient(&io::Error::from_raw_os_error(code)), "{code}");
        }
    }
}
