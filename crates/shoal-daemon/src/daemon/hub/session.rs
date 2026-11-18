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
    /// What the other side is, as failures name it: the hub, or a member.
    peer: String,
    /// The calls waiting for replies, none once the session ended, which
    /// fails them.
    pending: Mutex<Option<HashMap<u64, oneshot::Sender<Reply>>>>,
    next: AtomicU64,
}

impl Calls {
    pub(super) fn new(sender: Sender, peer: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            sender,
            peer: peer.into(),
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
            None => return Err(Unsent(format!("the link to {} closed", self.peer)).into()),
        };
        let sent_and_replied = async {
            let sent = super::pieces::send(&self.sender, &Frame::Call { id, call }, false).await;
            sent.map_err(|error| Unsent(format!("{error:#}")))?;
            Ok::<_, Unsent>(reply.await)
        };
        let reply = match tokio::time::timeout(CALL_TIMEOUT, sent_and_replied).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(unsent)) => {
                self.forget(id);
                return Err(unsent.into());
            }
            Err(timeout) => Err(timeout),
        };
        let peer = &self.peer;
        match reply {
            Ok(Ok(Reply::Failed { message })) => bail!("{message}"),
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => {
                Err(Unanswered(format!("the link to {peer} closed before it answered")).into())
            }
            Err(_) => {
                self.forget(id);
                let waited = CALL_TIMEOUT.as_secs();
                Err(Unanswered(format!("{peer} did not answer within {waited}s")).into())
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

/// A call that never left this side, which the other side never ran.
#[derive(Debug)]
pub(crate) struct Unsent(String);

/// A call the other side received but never answered, which it may have
/// run.
#[derive(Debug)]
pub(crate) struct Unanswered(String);

impl std::fmt::Display for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsent {}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unanswered {}

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
    let mut assembly = super::pieces::Assembly::default();
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
                    Ok(Some(Frame::Piece { id, data, more })) => {
                        match assembly.take(id, data, more) {
                            Ok(Some(frame)) => frame,
                            Ok(None) => continue,
                            Err(error) => return End::Lost(error),
                        }
                    }
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
                            let reply = Frame::Reply { id, reply };
                            let _ = super::pieces::send(&sender, &reply, true).await;
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
