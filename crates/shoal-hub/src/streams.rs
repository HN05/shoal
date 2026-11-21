//! Byte streams over a link, many at once beside its calls. The side that
//! opens a stream reads it, and names it in the call that asks the other
//! side to write it. Each stream has credit of its own: the writer sends
//! no more than the reader granted, and the reader grants more only as
//! whoever reads it takes what came, so a stream whose reader stalls holds
//! up neither the link's calls nor its other streams. A duplex stream also
//! carries the opener's bytes the other way, under credit of their own.
use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use shoal_protocol::hub::link::{Frame, STREAM_WINDOW};
use tokio::sync::{Semaphore, mpsc};

use crate::transport::Sender;
use shoal_core::fsutil::off_runtime;

/// The most bytes one [`Frame::Data`] carries.
const CHUNK: usize = 64 << 10;

/// The streams of one link, on one side.
pub struct Streams {
    sender: Sender,
    next: AtomicU64,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// Whether the link ended, which ends every stream.
    ended: bool,
    /// The directions of streams this side reads: those it opened, and
    /// the opener's bytes of the duplex ones the other side opened.
    reading: HashMap<Key, Reading>,
    /// The directions of streams this side writes: those the other side
    /// opened, and its own bytes of the duplex ones it opened.
    writing: HashMap<Key, Arc<Credit>>,
}

/// Which side opened a stream, whose number names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Opener {
    Here,
    There,
}

/// A stream as this side knows it: its number, and which side opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    stream: u64,
    opener: Opener,
}

impl Key {
    /// Whether frames about this side's direction of the stream, the one
    /// it writes when `writes`, concern the bytes its opener sends.
    fn up(self, writes: bool) -> bool {
        (self.opener == Opener::Here) == writes
    }

    /// The stream a frame from the other side names, sent by the writer of
    /// the direction it concerns when `from_writer`.
    fn named(stream: u64, up: bool, from_writer: bool) -> Self {
        let opener = match up == from_writer {
            true => Opener::There,
            false => Opener::Here,
        };
        Self { stream, opener }
    }

    /// The data frame of `bytes`, as this side writes them.
    fn data(self, bytes: &[u8]) -> Frame {
        Frame::Data {
            stream: self.stream,
            data: URL_SAFE_NO_PAD.encode(bytes),
            up: self.up(true),
        }
    }

    /// The end of what this side writes, with `error` when it failed.
    fn end(self, error: Option<String>) -> Frame {
        Frame::End {
            stream: self.stream,
            error,
            up: self.up(true),
        }
    }

    /// The credit of `bytes` more of what this side reads.
    fn credit(self, bytes: u64) -> Frame {
        Frame::Credit {
            stream: self.stream,
            bytes,
            up: self.up(false),
        }
    }

    /// That this side stopped reading.
    fn close(self) -> Frame {
        Frame::Close {
            stream: self.stream,
            up: self.up(false),
        }
    }
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
    pub fn new(sender: Sender) -> Self {
        Self {
            sender,
            next: AtomicU64::new(1),
            state: Arc::default(),
        }
    }

    /// A new stream this side reads, with the number the call that asks
    /// for it names.
    pub fn open(&self) -> Incoming {
        self.incoming(self.opened())
    }

    /// A new duplex stream, which this side reads and also writes, with
    /// the number the call that asks for it names.
    pub fn open_duplex(&self) -> (Incoming, Outgoing) {
        let key = self.opened();
        (self.incoming(key), self.outgoing(key))
    }

    /// This side's ends of the stream `stream` the other side opened and
    /// named in its call: its writer, and for a duplex one the reader of
    /// what the other side writes, taken before this side answers the
    /// call, which is when the other side starts writing.
    pub fn answering(&self, stream: u64, duplex: bool) -> (Outgoing, Option<Incoming>) {
        let key = Key {
            stream,
            opener: Opener::There,
        };
        (self.outgoing(key), duplex.then(|| self.incoming(key)))
    }

    /// The key of a new stream this side opens.
    fn opened(&self) -> Key {
        Key {
            stream: self.next.fetch_add(1, Ordering::Relaxed),
            opener: Opener::Here,
        }
    }

    /// The reader of `key`'s direction toward this side.
    fn incoming(&self, key: Key) -> Incoming {
        let (received, bytes) = mpsc::unbounded_channel();
        let reading = Reading {
            received: received.clone(),
            allowed: STREAM_WINDOW,
        };
        let mut state = lock(&self.state);
        match state.ended {
            true => drop(received.send(Received::End(Some("the link closed".into())))),
            false => drop(state.reading.insert(key, reading)),
        }
        Incoming {
            key,
            bytes,
            state: self.state.clone(),
            sender: self.sender.clone(),
            taken: 0,
            allowed_owed: false,
            ended: false,
        }
    }

    /// The writer of `key`'s direction from this side.
    fn outgoing(&self, key: Key) -> Outgoing {
        let credit = Arc::new(Credit {
            bytes: Semaphore::new(STREAM_WINDOW as usize),
            closed: tokio::sync::Notify::new(),
        });
        let mut state = lock(&self.state);
        match state.ended {
            true => credit.close(),
            false => drop(state.writing.insert(key, credit.clone())),
        }
        Outgoing {
            key,
            credit,
            state: self.state.clone(),
            sender: self.sender.clone(),
            ended: false,
        }
    }

    /// Take the frame `frame` for one of the link's streams, failing the
    /// link for a writer that sent past its credit; `false` for a frame of
    /// no stream.
    pub fn take(&self, frame: &Frame) -> Result<bool> {
        match frame {
            Frame::Data { stream, data, up } => {
                self.received(Key::named(*stream, *up, true), data)?
            }
            Frame::End { stream, error, up } => {
                self.ended(Key::named(*stream, *up, true), error.clone())
            }
            Frame::Credit { stream, bytes, up } => {
                self.credited(Key::named(*stream, *up, false), *bytes)
            }
            Frame::Close { stream, up } => self.closed(Key::named(*stream, *up, false)),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Pass the bytes `data` of the stream `key` to its reader, or tell
    /// the writer that no one reads it any longer.
    fn received(&self, key: Key, data: &str) -> Result<()> {
        let stream = key.stream;
        let bytes = URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|error| anyhow!("unreadable bytes of stream {stream}: {error}"))?;
        let mut state = lock(&self.state);
        let Some(reading) = state.reading.get_mut(&key) else {
            drop(state);
            send_soon(&self.sender, key.close());
            return Ok(());
        };
        let Some(allowed) = reading.allowed.checked_sub(bytes.len() as u64) else {
            bail!("stream {stream} sent more than its credit");
        };
        reading.allowed = allowed;
        let _ = reading.received.send(Received::Bytes(bytes));
        Ok(())
    }

    /// End the stream `key` this side reads, with `error`.
    fn ended(&self, key: Key, error: Option<String>) {
        if let Some(reading) = lock(&self.state).reading.remove(&key) {
            let _ = reading.received.send(Received::End(error));
        }
    }

    /// Let the writer of the stream `key` send `bytes` more.
    fn credited(&self, key: Key, bytes: u64) {
        if let Some(credit) = lock(&self.state).writing.get(&key) {
            let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
            let room = Semaphore::MAX_PERMITS - credit.bytes.available_permits();
            credit.bytes.add_permits(bytes.min(room));
        }
    }

    /// Stop writing the stream `key`, whose reader stopped.
    fn closed(&self, key: Key) {
        if let Some(credit) = lock(&self.state).writing.remove(&key) {
            credit.close();
        }
    }

    /// End every stream once the link ended, and open none after it.
    pub fn end(&self) {
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
pub struct Incoming {
    key: Key,
    bytes: mpsc::UnboundedReceiver<Received>,
    state: Arc<Mutex<State>>,
    sender: Sender,
    /// The bytes taken since this side last granted credit.
    taken: u64,
    /// Whether the bytes it owes credit for already count as allowed,
    /// while their credit waits to be sent.
    allowed_owed: bool,
    ended: bool,
}

impl Incoming {
    /// The number the call that asks for it names.
    pub fn number(&self) -> u64 {
        self.key.stream
    }

    /// The stream's next bytes, none once it ended; an error once its
    /// writer ended it with one or the link closed. Cancelling it, as
    /// `tokio::select!` does, loses nothing: it grants the credit the bytes
    /// it returned last earned before it waits for more, and holds them
    /// back from no one.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if self.ended {
            return Ok(None);
        }
        self.grant().await?;
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
        self.taken += bytes.len() as u64;
        Ok(Some(bytes))
    }

    /// Write what comes until the stream ends to `file`, then sync it,
    /// returning how many bytes came.
    pub async fn write_to(&mut self, mut file: std::fs::File) -> Result<u64> {
        let mut size = 0;
        while let Some(bytes) = self.next().await? {
            size += bytes.len() as u64;
            file = off_runtime::run(move || Ok(file.write_all(&bytes).map(|()| file)?)).await?;
        }
        off_runtime::run(move || Ok(file.sync_all()?)).await?;
        Ok(size)
    }

    /// Grant the writer the bytes taken again once they come to half the
    /// window, so it never waits for a reader that keeps up. A cancelled
    /// grant keeps what it owes for the next.
    async fn grant(&mut self) -> Result<()> {
        if self.taken < STREAM_WINDOW / 2 {
            return Ok(());
        }
        // Allowed first, so bytes the credit lets through never count as
        // past it.
        if !self.allowed_owed {
            if let Some(reading) = lock(&self.state).reading.get_mut(&self.key) {
                reading.allowed += self.taken;
            }
            self.allowed_owed = true;
        }
        self.sender.send(&self.key.credit(self.taken)).await?;
        (self.taken, self.allowed_owed) = (0, false);
        Ok(())
    }
}

impl Drop for Incoming {
    /// Tell the writer of a stream this side stops reading before its end.
    fn drop(&mut self) {
        if lock(&self.state).reading.remove(&self.key).is_some() {
            send_soon(&self.sender, self.key.close());
        }
    }
}

/// A stream this side writes: one the other side opened, or its own
/// bytes of a duplex one it opened.
pub struct Outgoing {
    key: Key,
    credit: Arc<Credit>,
    state: Arc<Mutex<State>>,
    sender: Sender,
    ended: bool,
}

impl Outgoing {
    /// Send `bytes`, waiting for credit as the reader grants it; failing
    /// once the reader stopped reading or the link closed.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.send_data(bytes, false).await
    }

    /// Send the contents of the file `path`, a chunk at a time.
    pub async fn write_file(&mut self, path: &Path) -> Result<()> {
        let path = path.to_owned();
        let mut input = off_runtime::run(move || {
            (std::fs::File::open(&path)).with_context(|| format!("open {}", path.display()))
        })
        .await?;
        loop {
            let (file, chunk) = off_runtime::run(move || {
                let mut chunk = vec![0; CHUNK];
                let read = input.read(&mut chunk)?;
                chunk.truncate(read);
                Ok((input, chunk))
            })
            .await?;
            if chunk.is_empty() {
                return Ok(());
            }
            input = file;
            self.write(&chunk).await?;
        }
    }

    /// [`Self::write`], returning once the bytes are written to the
    /// link's transport, as an answer that reports a change is before the
    /// daemon may exit.
    pub async fn write_through(&mut self, bytes: &[u8]) -> Result<()> {
        self.send_data(bytes, true).await
    }

    /// Send `bytes` as data frames, the last written to the transport
    /// before this returns when `through`.
    async fn send_data(&mut self, mut bytes: &[u8], through: bool) -> Result<()> {
        while !bytes.is_empty() {
            let size = self.take_credit(bytes.len().min(CHUNK)).await?;
            let (chunk, rest) = bytes.split_at(size);
            let data = self.key.data(chunk);
            match through && rest.is_empty() {
                true => self.sender.send_written(&data).await?,
                false => self.sender.send(&data).await?,
            }
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
    pub async fn closed(&self) {
        let closed = self.credit.closed.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self.credit.bytes.is_closed() {
            closed.await;
        }
    }

    /// End the stream, with `error` when it failed, returning once its end
    /// and so everything sent before it is written to the transport.
    pub async fn end(mut self, error: Option<String>) {
        self.ended = true;
        lock(&self.state).writing.remove(&self.key);
        let _ = self.sender.send_written(&self.key.end(error)).await;
    }
}

impl Drop for Outgoing {
    /// End a stream its writer left without ending it, as failed.
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        lock(&self.state).writing.remove(&self.key);
        send_soon(
            &self.sender,
            self.key.end(Some("its writer stopped".into())),
        );
    }
}

/// Send `frame`, which ends a stream for one side, without waiting: at
/// once while the link's send queue has room, else once it has, so the
/// other side hears of it while the link lasts and stops waiting for it.
fn send_soon(sender: &Sender, frame: Frame) {
    if sender.try_send(&frame).is_ok() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let sender = sender.clone();
    runtime.spawn(async move { sender.send(&frame).await });
}

/// Write what `incoming` brings to `outgoing` until it ends, ending
/// `outgoing` as `incoming` ended; once no one reads `outgoing`, stop
/// reading `incoming`.
pub async fn relay(mut incoming: Incoming, mut outgoing: Outgoing) {
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
