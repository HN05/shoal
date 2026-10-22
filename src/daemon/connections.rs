//! Accepting client connections, the capacity they hold and the descriptors
//! they use.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

use super::log;
use crate::protocol::Method;

/// Most connections reading their request or running a request/response
/// operation at once.
const MAX_REQUESTS: usize = 128;
/// Descriptors kept for the daemon's own files and the commands it runs.
const RESERVED_DESCRIPTORS: usize = 512;
/// Long-lived connections allowed even when the descriptor limit is low.
const MIN_LONG_LIVED: usize = 64;

/// Separate limits for requests and for connections that stay open, such as
/// executions and waits, so those cannot take the capacity of the requests
/// that end them.
pub(super) struct Capacity {
    requests: Arc<Semaphore>,
    long_lived: Arc<Semaphore>,
    long_lived_limit: usize,
    /// Set from the first refusal until a long-lived connection is admitted
    /// again, so retrying clients log the limit once.
    long_lived_full: AtomicBool,
}

/// Capacity a connection holds until it closes.
pub(super) struct Slot {
    _permit: OwnedSemaphorePermit,
}

impl Capacity {
    /// Long-lived connections get what the descriptor limit leaves after
    /// requests and the daemon's own files.
    pub(super) fn for_descriptors(limit: u64) -> Self {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let long_lived = limit
            .saturating_sub(MAX_REQUESTS + RESERVED_DESCRIPTORS)
            .clamp(MIN_LONG_LIVED, Semaphore::MAX_PERMITS);
        Self::new(MAX_REQUESTS, long_lived)
    }

    pub(super) fn new(requests: usize, long_lived: usize) -> Self {
        Self {
            requests: Arc::new(Semaphore::new(requests)),
            long_lived: Arc::new(Semaphore::new(long_lived)),
            long_lived_limit: long_lived,
            long_lived_full: AtomicBool::new(false),
        }
    }

    /// The next client connection, once a request slot is free.
    pub(super) async fn accept(&self, listener: &UnixListener) -> io::Result<(UnixStream, Slot)> {
        let permit = self
            .requests
            .clone()
            .acquire_owned()
            .await
            .expect("request capacity is never closed");
        Ok((accept(listener).await?, Slot { _permit: permit }))
    }

    /// The slot a connection keeps once its request is known. One that stays
    /// open moves into long-lived capacity, freeing its request slot.
    pub(super) fn admit(&self, request: Slot, method: &Method) -> Result<Slot> {
        if !method.holds_connection() {
            return Ok(request);
        }
        let Ok(permit) = self.long_lived.clone().try_acquire_owned() else {
            if !self.long_lived_full.swap(true, Ordering::Relaxed) {
                log!(
                    "refusing long-lived connections at the limit of {}",
                    self.long_lived_limit
                );
            }
            return Err(anyhow!(
                "the daemon already serves its limit of {} long-lived connections",
                self.long_lived_limit
            ));
        };
        self.long_lived_full.store(false, Ordering::Relaxed);
        drop(request);
        Ok(Slot { _permit: permit })
    }

    /// A request slot for a connection served without a listener.
    #[cfg(test)]
    pub(super) fn request_slot(&self) -> Slot {
        let permit = self.requests.clone().try_acquire_owned();
        Slot {
            _permit: permit.expect("a free request slot"),
        }
    }
}

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
async fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
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
    use std::time::Instant;

    use tokio::{
        io::{AsyncReadExt, BufReader},
        sync::watch,
    };

    use super::*;
    use crate::{
        daemon::{Server, questions::Ask, serve},
        protocol::{self, Body, Request, Response},
        test_support::{manager, repository},
    };

    /// A hang guard, not the behavior under test.
    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(60), future)
            .await
            .expect("connection exchange timed out")
    }

    async fn send(socket: &std::path::Path, method: Method) -> BufReader<UnixStream> {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        let request = Request {
            protocol: protocol::VERSION,
            id: 1,
            method,
            scope: None,
        };
        protocol::write(&mut stream, &request).await.unwrap();
        BufReader::new(stream)
    }

    async fn reply(stream: &mut BufReader<UnixStream>) -> Body {
        let response: Response = bounded(protocol::read_buffered(stream)).await.unwrap();
        response.body
    }

    #[tokio::test]
    async fn requests_are_served_while_long_lived_capacity_is_full() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "asker".into(), None, None, None)
            .await
            .unwrap();
        let ask = Ask {
            question: "Which database?".into(),
            recommendation: "SQLite".into(),
            reason: None,
            choices: Vec::new(),
        };
        let question = manager
            .ask_question(&workspace.name, ask.clone())
            .await
            .unwrap();
        let socket = &manager.paths.socket.clone();
        let listener = UnixListener::bind(socket).unwrap();
        // With one request slot, the next connection is accepted only once
        // the wait before it has moved into long-lived capacity.
        let server = Server {
            manager: manager.clone(),
            started: Instant::now(),
            managed: false,
            shutdown: watch::channel(false).0,
            capacity: Arc::new(Capacity::new(1, 1)),
        };
        let serving = tokio::spawn(async move {
            loop {
                let (stream, slot) = server.capacity.accept(&listener).await.unwrap();
                let server = server.clone();
                let operation = manager.background_operations.clone().read_owned().await;
                tokio::spawn(serve(stream, server, slot, operation));
            }
        });
        let await_answer = || Method::AwaitAnswer {
            workspace: workspace.name.clone(),
            id: question.id,
            ask: ask.clone(),
        };
        let mut waiting = send(socket, await_answer()).await;

        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        let execute = Method::Execute {
            workspace: workspace.name.clone(),
            wrapper: wrapper.clone(),
            kind: crate::daemon::workspace::ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: true,
            queue_entry: None,
        };
        let watch = Method::WatchNotifications {
            keep_unread: false,
            after: 0,
        };
        for refused in [execute, watch] {
            let Body::Error { message, .. } = reply(&mut send(socket, refused).await).await else {
                panic!("expected a refusal");
            };
            assert!(message.contains("limit of 1 long-lived"), "{message}");
        }
        let reattach = Method::Reattach(protocol::Reattach {
            execution: "running".into(),
            wrapper,
            child: None,
            group_id: std::process::id(),
            scope_token: "token".into(),
            agent: None,
            recover: false,
            running_ms: 0,
            awaiting_restore: None,
        });
        // Clients that retry a connection closed unanswered keep waiting.
        for retried in [await_answer(), reattach] {
            let mut unanswered = Vec::new();
            bounded(send(socket, retried).await.read_to_end(&mut unanswered))
                .await
                .unwrap();
            assert!(
                unanswered.is_empty(),
                "{}",
                String::from_utf8_lossy(&unanswered)
            );
        }
        let status = reply(&mut send(socket, Method::Status).await).await;
        assert!(matches!(status, Body::Status(_)), "{status:?}");
        let answer = Method::AnswerQuestion {
            id: question.id,
            answer: "SQLite".into(),
            shown: None,
        };
        let answered = reply(&mut send(socket, answer).await).await;
        assert!(matches!(answered, Body::Question(_)), "{answered:?}");
        let Body::Question(delivered) = reply(&mut waiting).await else {
            panic!("expected the wait to receive the answer");
        };
        assert_eq!(delivered.answer.as_deref(), Some("SQLite"));
        serving.abort();
    }

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
