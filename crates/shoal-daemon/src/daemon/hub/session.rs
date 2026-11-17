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
use tokio::{sync::oneshot, time::Instant};

use super::transport::{Channel, Sender};

/// How long a call waits for its reply.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The calls this side sends over a link, each waiting for its reply.
pub(crate) struct Calls {
    sender: Sender,
    /// The calls waiting for replies, none once the session ended, which
    /// fails them.
    pending: Mutex<Option<HashMap<u64, oneshot::Sender<Reply>>>>,
    next: AtomicU64,
}

impl Calls {
    pub(super) fn new(sender: Sender) -> Arc<Self> {
        Arc::new(Self {
            sender,
            pending: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(1),
        })
    }

    /// The other side's reply to `call`, failing as [`Reply::Failed`] does.
    pub(crate) async fn call(&self, call: Call) -> Result<Reply> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (replied, reply) = oneshot::channel();
        match self.pending.lock().unwrap().as_mut() {
            Some(pending) => pending.insert(id, replied),
            None => bail!("the link to the hub closed"),
        };
        let sent_and_replied = async {
            self.sender.send(&Frame::Call { id, call }).await?;
            anyhow::Ok(reply.await)
        };
        let reply = match tokio::time::timeout(CALL_TIMEOUT, sent_and_replied).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => {
                self.forget(id);
                return Err(error);
            }
            Err(timeout) => Err(timeout),
        };
        match reply {
            Ok(Ok(Reply::Failed { message })) => bail!("{message}"),
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => bail!("the link to the hub closed before it answered"),
            Err(_) => {
                self.forget(id);
                bail!("the hub did not answer within {}s", CALL_TIMEOUT.as_secs())
            }
        }
    }

    fn replied(&self, id: u64, reply: Reply) {
        let waiting = self
            .pending
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|pending| pending.remove(&id));
        if let Some(waiting) = waiting {
            let _ = waiting.send(reply);
        }
    }

    fn forget(&self, id: u64) {
        if let Some(pending) = self.pending.lock().unwrap().as_mut() {
            pending.remove(&id);
        }
    }

    /// Fail every call waiting for a reply, and refuse those made later,
    /// once the session ended.
    fn close(&self) {
        self.pending.lock().unwrap().take();
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
/// side's calls with `answer` and passing replies to `calls`. An answer
/// comes with what it holds until its reply is written, such as the daemon
/// operation that keeps an update from exiting between a change and the
/// reply that reports it.
pub(super) async fn serve<A, F, H>(
    channel: Channel,
    calls: &Calls,
    answer: A,
    stop: impl Future<Output = ()>,
) -> End
where
    A: Fn(Call) -> F,
    F: Future<Output = (Reply, H)> + Send + 'static,
    H: Send + 'static,
{
    let end = exchange(channel, calls, answer, stop).await;
    calls.close();
    end
}

/// Exchange frames on `channel` until it ends or `stop` resolves.
async fn exchange<A, F, H>(
    mut channel: Channel,
    calls: &Calls,
    answer: A,
    stop: impl Future<Output = ()>,
) -> End
where
    A: Fn(Call) -> F,
    F: Future<Output = (Reply, H)> + Send + 'static,
    H: Send + 'static,
{
    let sender = channel.sender();
    let mut pings = tokio::time::interval(SILENCE / 3);
    let mut heard = Instant::now();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return End::Stopped,
            _ = pings.tick() => {
                if let Err(error) = sender.try_send(&Frame::Ping) {
                    return End::Lost(error);
                }
            }
            () = tokio::time::sleep_until(heard + SILENCE) => {
                return End::Lost(anyhow!("heard nothing for {}s", SILENCE.as_secs()));
            }
            frame = channel.receive::<Frame>() => {
                heard = Instant::now();
                let frame = match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return End::Closed,
                    Err(error) => return End::Lost(error),
                };
                match frame {
                    Frame::Ping => {
                        if let Err(error) = sender.try_send(&Frame::Pong) {
                            return End::Lost(error);
                        }
                    }
                    Frame::Pong => {}
                    Frame::Reply { id, reply } => calls.replied(id, reply),
                    Frame::Call { id, call } => {
                        let (answer, sender) = (answer(call), sender.clone());
                        // Detached: an answer that changes something finishes,
                        // and drops what it holds, even once the link ends.
                        tokio::spawn(async move {
                            let (reply, held) = answer.await;
                            let _ = sender.send_written(&Frame::Reply { id, reply }).await;
                            drop(held);
                        });
                    }
                    frame => {
                        let frame = super::shown(&format!("{frame:?}"));
                        return End::Lost(anyhow!("unexpected hub link frame {frame}"));
                    }
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

#[cfg(test)]
mod tests;
