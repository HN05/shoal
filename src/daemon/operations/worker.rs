//! An operation's worker: this binary's operation worker, started detached
//! from the daemon as queued agents are, reading its order on standard
//! input, with what it and its processes write relayed as the operation's
//! output.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::Entry;
use crate::{
    paths::Paths,
    protocol::operations::{Order, Stream},
};

/// How long output keeps arriving after the worker exited, from processes
/// it left behind, before the operation ends without it.
const DRAIN: Duration = Duration::from_secs(1);

type Output = Box<dyn AsyncRead + Send + Unpin>;
/// Interrupts a worker that has not exited.
pub(super) type Interrupt = Box<dyn Fn() + Send + Sync>;

/// A started worker: its standard output and error, its exit status, and
/// what interrupts it.
pub(super) struct Worker {
    stdout: Output,
    stderr: Output,
    exit: BoxFuture<'static, i32>,
    pub interrupt: Option<Interrupt>,
}

/// Start the worker of `order` in a new session, in `env` instead of the
/// daemon's environment when given, as the user or, with `token`, with that
/// scope.
#[cfg(not(test))]
pub(super) fn spawn(
    paths: &Paths,
    order: &Order,
    env: Option<BTreeMap<String, String>>,
    token: Option<&str>,
) -> anyhow::Result<Worker> {
    use anyhow::Context as _;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt as _;

    use crate::cli::internal::{Worker as Internal, internal_command};

    let argv = internal_command(paths, false, Internal::Operation)?;
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&paths.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    match token {
        Some(token) => command.env(crate::env::SCOPE_TOKEN, token),
        None => command.env_remove(crate::env::SCOPE_TOKEN),
    };
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start the operation's worker")?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        anyhow::bail!("the operation's worker has no standard streams");
    };
    let pid = child
        .id()
        .context("the operation's worker has no process ID")? as i32;
    let order = serde_json::to_vec(order)?;
    // The worker reads its order, then its processes find standard input at
    // its end.
    tokio::spawn(async move {
        let _ = stdin.write_all(&order).await;
    });
    Ok(Worker {
        stdout: Box::new(stdout),
        stderr: Box::new(stderr),
        exit: Box::pin(async move {
            match child.wait().await {
                Ok(status) => crate::execution::exit_code(status),
                Err(_) => 1,
            }
        }),
        // The worker's tracked executions stop their commands on SIGTERM.
        // SAFETY: signalling a process ID, which the worker keeps until
        // the operation's end forgets this.
        interrupt: Some(Box::new(move || unsafe {
            libc::kill(pid, libc::SIGTERM);
        })),
    })
}

/// Tests start a [`fake`] worker instead of running this binary.
#[cfg(test)]
pub(super) fn spawn(
    _paths: &Paths,
    order: &Order,
    _env: Option<BTreeMap<String, String>>,
    token: Option<&str>,
) -> anyhow::Result<Worker> {
    Ok(fake::start(order, token))
}

/// Relay the worker's output to `entry` until it exits and its streams
/// close, or [`DRAIN`] after it exits, then end the operation.
pub(super) async fn supervise(entry: Arc<Entry>, worker: Worker) {
    let stdout = tokio::spawn(relay(entry.clone(), worker.stdout, Stream::Stdout));
    let stderr = tokio::spawn(relay(entry.clone(), worker.stderr, Stream::Stderr));
    let code = worker.exit.await;
    entry.update(|state| state.interrupt = None);
    let (stdout_abort, stderr_abort) = (stdout.abort_handle(), stderr.abort_handle());
    if tokio::time::timeout(DRAIN, async {
        let _ = tokio::join!(stdout, stderr);
    })
    .await
    .is_err()
    {
        stdout_abort.abort();
        stderr_abort.abort();
    }
    entry.end(code);
}

/// Record each line `output` holds as written to `stream`, until it closes.
async fn relay(entry: Arc<Entry>, output: Output, stream: Stream) {
    let mut lines = BufReader::new(output).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        entry.output(stream, &line);
    }
}

/// A worker tests drive: they write its output and choose its exit.
#[cfg(test)]
pub(crate) mod fake {
    use std::{
        collections::HashMap,
        sync::{
            Arc, LazyLock, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use tokio::io::{AsyncWriteExt, DuplexStream};

    use super::Worker;
    use crate::protocol::operations::Order;

    static STARTED: LazyLock<Mutex<HashMap<String, Fake>>> = LazyLock::new(Default::default);

    /// A started fake worker, by its operation's ID.
    pub(crate) struct Fake {
        pub order: Order,
        pub token: Option<String>,
        pub interrupted: Arc<AtomicBool>,
        stdout: DuplexStream,
        stderr: DuplexStream,
        exit: tokio::sync::oneshot::Sender<i32>,
    }

    pub(super) fn start(order: &Order, token: Option<&str>) -> Worker {
        let (stdout, stdout_end) = tokio::io::duplex(64 * 1024);
        let (stderr, stderr_end) = tokio::io::duplex(64 * 1024);
        let (exit, exited) = tokio::sync::oneshot::channel();
        let interrupted = Arc::new(AtomicBool::new(false));
        STARTED.lock().unwrap().insert(
            order.id.clone(),
            Fake {
                order: order.clone(),
                token: token.map(str::to_owned),
                interrupted: interrupted.clone(),
                stdout,
                stderr,
                exit,
            },
        );
        Worker {
            stdout: Box::new(stdout_end),
            stderr: Box::new(stderr_end),
            exit: Box::pin(async move { exited.await.unwrap_or(1) }),
            interrupt: Some(Box::new(move || interrupted.store(true, Ordering::SeqCst))),
        }
    }

    /// The fake worker of operation `id`.
    pub(crate) fn take(id: &str) -> Fake {
        STARTED
            .lock()
            .unwrap()
            .remove(id)
            .expect("the operation started a worker")
    }

    impl Fake {
        pub(crate) async fn stdout(&mut self, line: &str) {
            self.stdout
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }

        pub(crate) async fn stderr(&mut self, line: &str) {
            self.stderr
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }

        /// Exit with `code`, closing its streams.
        pub(crate) fn exit(self, code: i32) {
            let _ = self.exit.send(code);
        }
    }
}
