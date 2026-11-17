//! An open link after its handshake, on either side: calls to the other side
//! and their replies, calls from it, and the pings that tell a link that went
//! silent, as one whose network dropped away, from a quiet one.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use shoal_protocol::hub::link::{Call, Frame, Reply, SILENCE};
use tokio::{sync::oneshot, task::JoinSet, time::Instant};

use super::transport::{Channel, Sender};

/// How long a call waits for its reply.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The calls this side sends over a link, each waiting for its reply.
pub(crate) struct Calls {
    sender: Sender,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next: AtomicU64,
}

impl Calls {
    pub(super) fn new(sender: Sender) -> Arc<Self> {
        Arc::new(Self {
            sender,
            pending: Mutex::default(),
            next: AtomicU64::new(1),
        })
    }

    /// The other side's reply to `call`, failing as [`Reply::Failed`] does.
    pub(crate) async fn call(&self, call: Call) -> Result<Reply> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (replied, reply) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, replied);
        let sent = self.sender.send(&Frame::Call { id, call }).await;
        let reply = match sent {
            Ok(()) => tokio::time::timeout(CALL_TIMEOUT, reply).await,
            Err(error) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(error);
            }
        };
        match reply {
            Ok(Ok(Reply::Failed { message })) => bail!("{message}"),
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => bail!("the link to the hub closed before it answered"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("the hub did not answer within {}s", CALL_TIMEOUT.as_secs())
            }
        }
    }

    fn replied(&self, id: u64, reply: Reply) {
        if let Some(waiting) = self.pending.lock().unwrap().remove(&id) {
            let _ = waiting.send(reply);
        }
    }
}

/// How a session ended.
pub(super) enum End {
    /// The other side closed the link.
    Closed,
    /// It failed, or the other side went silent for [`SILENCE`].
    Lost(anyhow::Error),
    /// This side stopped it.
    Stopped,
}

/// Serve `channel` until it ends or `stop` resolves, answering the other
/// side's calls with `answer` and passing replies to `calls`.
pub(super) async fn serve<A, F>(
    mut channel: Channel,
    calls: &Calls,
    answer: A,
    stop: impl Future<Output = ()>,
) -> End
where
    A: Fn(Call) -> F,
    F: Future<Output = Reply> + Send + 'static,
{
    let sender = channel.sender();
    let mut answering = JoinSet::new();
    let mut pings = tokio::time::interval(SILENCE / 3);
    let mut heard = Instant::now();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return End::Stopped,
            _ = pings.tick() => {
                if let Err(error) = sender.send(&Frame::Ping).await {
                    return End::Lost(error);
                }
            }
            () = tokio::time::sleep_until(heard + SILENCE) => {
                return End::Lost(anyhow!("heard nothing for {}s", SILENCE.as_secs()));
            }
            Some(_) = answering.join_next(), if !answering.is_empty() => {}
            frame = channel.receive::<Frame>() => {
                heard = Instant::now();
                let frame = match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return End::Closed,
                    Err(error) => return End::Lost(error),
                };
                match frame {
                    Frame::Ping => {
                        if let Err(error) = sender.send(&Frame::Pong).await {
                            return End::Lost(error);
                        }
                    }
                    Frame::Pong => {}
                    Frame::Reply { id, reply } => calls.replied(id, reply),
                    Frame::Call { id, call } => {
                        let (reply, sender) = (answer(call), sender.clone());
                        answering.spawn(async move {
                            let _ = sender.send(&Frame::Reply { id, reply: reply.await }).await;
                        });
                    }
                    frame => return End::Lost(anyhow!("unexpected hub link frame {frame:?}")),
                }
            }
        }
    }
}

/// The reply a call that failed with `result`'s error gets.
pub(super) fn reply(result: Result<Reply>) -> Reply {
    result.unwrap_or_else(|error| Reply::Failed {
        message: format!("{error:#}"),
    })
}
