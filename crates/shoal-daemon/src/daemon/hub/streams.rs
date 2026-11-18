//! Byte streams over a link, many at once beside its calls. The side that
//! opens a stream reads it, and names it in the call that asks the other
//! side to write it. Each stream has credit of its own: the writer sends
//! no more than the reader granted, and the reader grants more only as
//! whoever reads it takes what came, so a stream whose reader stalls holds
//! up neither the link's calls nor its other streams.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use shoal_protocol::hub::link::{Frame, STREAM_WINDOW};
use tokio::sync::{Semaphore, mpsc};

use super::transport::Sender;

/// The most bytes one [`Frame::Data`] carries.
const CHUNK: usize = 64 << 10;

/// The streams of one link, on one side.
pub(crate) struct Streams {
    sender: Sender,
    next: AtomicU64,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// Whether the link ended, which ends every stream.
    ended: bool,
    /// The streams this side opened and reads, by number.
    reading: HashMap<u64, Reading>,
    /// The streams the other side opened, which this side writes.
    writing: HashMap<u64, Arc<Credit>>,
}

/// The credit a stream this side writes has left, closed once its reader
/// stopped reading it or the link ended.
struct Credit {
    bytes: Semaphore,
    /// Wakes whoever waits for it to close.
    closed: tokio::sync::Notify,
}

impl Credit {
    fn close(&self) {
        self.bytes.close();
        self.closed.notify_waiters();
    }
}

/// What a stream this side reads holds while it is open.
struct Reading {
    /// Where its bytes go, for [`Incoming`] to take.
    received: mpsc::UnboundedSender<Received>,
    /// The bytes its writer may still send.
    allowed: u64,
}

/// What came of a stream this side reads.
enum Received {
    Bytes(Vec<u8>),
    End(Option<String>),
}

/// The lock on `state`, which no panic holding it leaves inconsistent.
fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Streams {
    pub(super) fn new(sender: Sender) -> Self {
        Self {
            sender,
            next: AtomicU64::new(1),
            state: Arc::default(),
        }
    }

    /// A new stream this side reads, with the number the call that asks
    /// for it names.
    pub(super) fn open(&self) -> Incoming {
        let stream = self.next.fetch_add(1, Ordering::Relaxed);
        let (received, bytes) = mpsc::unbounded_channel();
        let reading = Reading {
            received: received.clone(),
            allowed: STREAM_WINDOW,
        };
        let mut state = lock(&self.state);
        match state.ended {
            true => drop(received.send(Received::End(Some("the link closed".into())))),
            false => drop(state.reading.insert(stream, reading)),
        }
        Incoming {
            stream,
            bytes,
            state: self.state.clone(),
            sender: self.sender.clone(),
            taken: 0,
            ended: false,
        }
    }

    /// The writer of the stream `stream` the other side opened and named
    /// in its call.
    pub(crate) fn writer(&self, stream: u64) -> Outgoing {
        let credit = Arc::new(Credit {
            bytes: Semaphore::new(STREAM_WINDOW as usize),
            closed: tokio::sync::Notify::new(),
        });
        let mut state = lock(&self.state);
        match state.ended {
            true => credit.close(),
            false => drop(state.writing.insert(stream, credit.clone())),
        }
        Outgoing {
            stream,
            credit,
            state: self.state.clone(),
            sender: self.sender.clone(),
            ended: false,
        }
    }

    /// Take the frame `frame` for one of the link's streams, failing the
    /// link for a writer that sent past its credit; `false` for a frame of
    /// no stream.
    pub(super) fn take(&self, frame: &Frame) -> Result<bool> {
        match frame {
            Frame::Data { stream, data } => self.received(*stream, data)?,
            Frame::End { stream, error } => self.ended(*stream, error.clone()),
            Frame::Credit { stream, bytes } => self.credited(*stream, *bytes),
            Frame::Close { stream } => self.closed(*stream),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Pass the bytes `data` of the stream `stream` to its reader, or
    /// tell the writer that no one reads it any longer.
    fn received(&self, stream: u64, data: &str) -> Result<()> {
        let bytes = URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|error| anyhow!("unreadable bytes of stream {stream}: {error}"))?;
        let mut state = lock(&self.state);
        let Some(reading) = state.reading.get_mut(&stream) else {
            drop(state);
            let _ = self.sender.try_send(&Frame::Close { stream });
            return Ok(());
        };
        let Some(allowed) = reading.allowed.checked_sub(bytes.len() as u64) else {
            bail!("stream {stream} sent more than its credit");
        };
        reading.allowed = allowed;
        let _ = reading.received.send(Received::Bytes(bytes));
        Ok(())
    }

    /// End the stream `stream` this side reads, with `error`.
    fn ended(&self, stream: u64, error: Option<String>) {
        if let Some(reading) = lock(&self.state).reading.remove(&stream) {
            let _ = reading.received.send(Received::End(error));
        }
    }

    /// Let the writer of the stream `stream` send `bytes` more.
    fn credited(&self, stream: u64, bytes: u64) {
        if let Some(credit) = lock(&self.state).writing.get(&stream) {
            let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
            let room = Semaphore::MAX_PERMITS - credit.bytes.available_permits();
            credit.bytes.add_permits(bytes.min(room));
        }
    }

    /// Stop writing the stream `stream`, whose reader stopped.
    fn closed(&self, stream: u64) {
        if let Some(credit) = lock(&self.state).writing.remove(&stream) {
            credit.close();
        }
    }

    /// End every stream once the link ended, and open none after it.
    pub(super) fn end(&self) {
        let mut state = lock(&self.state);
        state.ended = true;
        for (_, reading) in state.reading.drain() {
            let _ = (reading.received).send(Received::End(Some("the link closed".into())));
        }
        for (_, credit) in state.writing.drain() {
            credit.close();
        }
    }
}

/// A stream this side reads.
pub(crate) struct Incoming {
    stream: u64,
    bytes: mpsc::UnboundedReceiver<Received>,
    state: Arc<Mutex<State>>,
    sender: Sender,
    /// The bytes taken since this side last granted credit.
    taken: u64,
    ended: bool,
}

impl Incoming {
    /// The number the call that asks for it names.
    pub(super) fn number(&self) -> u64 {
        self.stream
    }

    /// The stream's next bytes, none once it ended; an error once its
    /// writer ended it with one or the link closed.
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if self.ended {
            return Ok(None);
        }
        let received = self.bytes.recv().await;
        let bytes = match received {
            Some(Received::Bytes(bytes)) => bytes,
            Some(Received::End(None)) => {
                self.ended = true;
                return Ok(None);
            }
            Some(Received::End(Some(error))) => {
                self.ended = true;
                bail!("{error}");
            }
            None => {
                self.ended = true;
                bail!("the link closed");
            }
        };
        self.grant(bytes.len() as u64).await?;
        Ok(Some(bytes))
    }

    /// Count `bytes` taken, and grant the writer that much again once it
    /// comes to half the window, so it never waits for a reader that keeps
    /// up.
    async fn grant(&mut self, bytes: u64) -> Result<()> {
        self.taken += bytes;
        if self.taken < STREAM_WINDOW / 2 {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.taken);
        // Allowed first, so bytes the credit lets through never count as
        // past it.
        if let Some(reading) = lock(&self.state).reading.get_mut(&self.stream) {
            reading.allowed += bytes;
        }
        let credit = Frame::Credit {
            stream: self.stream,
            bytes,
        };
        self.sender.send(&credit).await
    }
}

impl Drop for Incoming {
    /// Tell the writer of a stream this side stops reading before its end.
    fn drop(&mut self) {
        if lock(&self.state).reading.remove(&self.stream).is_some() {
            let _ = self.sender.try_send(&Frame::Close {
                stream: self.stream,
            });
        }
    }
}

/// A stream the other side opened, which this side writes.
pub(crate) struct Outgoing {
    stream: u64,
    credit: Arc<Credit>,
    state: Arc<Mutex<State>>,
    sender: Sender,
    ended: bool,
}

impl Outgoing {
    /// Send `bytes`, waiting for credit as the reader grants it; failing
    /// once the reader stopped reading or the link closed.
    pub(crate) async fn write(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let size = self.take_credit(bytes.len().min(CHUNK)).await?;
            let (chunk, rest) = bytes.split_at(size);
            let data = Frame::Data {
                stream: self.stream,
                data: URL_SAFE_NO_PAD.encode(chunk),
            };
            self.sender.send(&data).await?;
            bytes = rest;
        }
        Ok(())
    }

    /// Up to `wanted` bytes of credit, at least one, once the reader
    /// granted any.
    async fn take_credit(&self, wanted: usize) -> Result<usize> {
        let bytes = &self.credit.bytes;
        let stopped = || anyhow!("the stream's reader stopped reading it");
        bytes.acquire().await.map_err(|_| stopped())?.forget();
        let more = (wanted - 1).min(bytes.available_permits());
        if let Ok(permits) = bytes.try_acquire_many(more as u32) {
            permits.forget();
            return Ok(1 + more);
        }
        Ok(1)
    }

    /// Resolves once the reader stopped reading the stream or the link
    /// ended, so that whatever feeds it stops too.
    pub(crate) async fn closed(&self) {
        let closed = self.credit.closed.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self.credit.bytes.is_closed() {
            closed.await;
        }
    }

    /// End the stream, with `error` when it failed.
    pub(crate) async fn end(mut self, error: Option<String>) {
        self.ended = true;
        lock(&self.state).writing.remove(&self.stream);
        let end = Frame::End {
            stream: self.stream,
            error,
        };
        let _ = self.sender.send(&end).await;
    }
}

impl Drop for Outgoing {
    /// End a stream its writer left without ending it, as failed.
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        lock(&self.state).writing.remove(&self.stream);
        let _ = self.sender.try_send(&Frame::End {
            stream: self.stream,
            error: Some("its writer stopped".into()),
        });
    }
}

/// Write what `incoming` brings to `outgoing` until it ends, ending
/// `outgoing` as `incoming` ended; once no one reads `outgoing`, stop
/// reading `incoming`.
pub(super) async fn relay(mut incoming: Incoming, mut outgoing: Outgoing) {
    let error = loop {
        let next = tokio::select! {
            next = incoming.next() => next,
            () = outgoing.closed() => return,
        };
        match next {
            Ok(Some(bytes)) => {
                if outgoing.write(&bytes).await.is_err() {
                    return;
                }
            }
            Ok(None) => break None,
            Err(error) => break Some(format!("{error:#}")),
        }
    };
    outgoing.end(error).await;
}

#[cfg(test)]
mod tests;
