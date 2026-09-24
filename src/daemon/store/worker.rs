//! One connection owned by a blocking thread, behind a bounded queue.
use super::*;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

const QUEUE_CAPACITY: usize = 128;
type Job = Box<dyn FnOnce(&mut Option<Connection>, &Path) + Send>;

pub(super) struct Worker {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    stopped: watch::Receiver<()>,
    #[cfg(test)]
    pub(super) opens: Arc<std::sync::atomic::AtomicU64>,
}

impl Worker {
    pub(super) fn start(path: PathBuf) -> Result<Arc<Self>> {
        let (sender, mut receiver) = mpsc::channel::<Job>(QUEUE_CAPACITY);
        let (finished, stopped) = watch::channel(());
        std::thread::Builder::new()
            .name("shoal-sqlite".into())
            .spawn(move || {
                // Drop the completion sender only after SQLite has closed.
                let _finished = finished;
                let mut connection = None;
                while let Some(job) = receiver.blocking_recv() {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        job(&mut connection, &path);
                    }));
                    // A panic or an unfinished raw transaction must not leak state
                    // into the next operation. Never replay the failed operation.
                    if outcome.is_err() || connection.as_ref().is_some_and(|db| !db.is_autocommit())
                    {
                        connection = None;
                    }
                }
            })
            .context("start database worker")?;
        Ok(Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            stopped,
            #[cfg(test)]
            opens: Arc::default(),
        }))
    }

    pub(super) async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (send, receive) = oneshot::channel();
        #[cfg(test)]
        let opens = self.opens.clone();
        let job = Box::new(move |connection: &mut Option<Connection>, path: &Path| {
            let result = (|| {
                if connection.is_none() {
                    *connection = Some(connect(path)?);
                    #[cfg(test)]
                    opens.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                operation(connection.as_mut().expect("connection was opened"))
            })();
            let _ = send.send(result);
        });
        // Callers are bounded by the daemon's client limit, so waiting for queue
        // space applies backpressure instead of failing ordinary requests.
        let sender = self
            .sender
            .lock()
            .unwrap()
            .clone()
            .context("database worker is shut down")?;
        sender
            .send(job)
            .await
            .map_err(|_| anyhow::anyhow!("database worker failed"))?;
        receive.await.context("database worker failed")?
    }

    /// Stop accepting work and wait for admitted operations and connection close.
    /// Cancellation of a caller does not cancel or retry its admitted operation.
    pub(super) async fn shutdown(&self) {
        self.sender.lock().unwrap().take();
        let mut stopped = self.stopped.clone();
        let _ = stopped.changed().await;
    }
}

fn connect(path: &Path) -> Result<Connection> {
    let db = Connection::open(path).context("open Shoal state database")?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(db)
}

#[cfg(test)]
mod tests;
