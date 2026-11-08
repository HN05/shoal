//! A worker tests drive: they write its output and choose its exit.
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
