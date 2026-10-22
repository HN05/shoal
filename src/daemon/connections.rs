//! Accepting client connections.

use std::{io, time::Duration};

use tokio::net::{UnixListener, UnixStream};

use super::log;

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
    fn descriptor_exhaustion_and_aborted_clients_are_transient() {
        for code in [libc::EMFILE, libc::ENFILE, libc::ECONNABORTED, libc::EINTR] {
            assert!(transient(&io::Error::from_raw_os_error(code)), "{code}");
        }
        for code in [libc::EBADF, libc::EINVAL] {
            assert!(!transient(&io::Error::from_raw_os_error(code)), "{code}");
        }
    }
}
