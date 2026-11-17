//! A link's transport: its frames as lines of a byte stream, such as the
//! daemon's socket or a command's standard input and output, or as text
//! messages of a WebSocket. Each direction runs in a task of its own, so
//! that waiting for a frame never holds up sending one. The reader stops
//! with its channel; the writer once every sender is gone, after sending
//! what they queued. Once sealed, the writer seals each frame it sends and
//! the channel opens each it receives.
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use futures_util::{Sink, SinkExt as _, StreamExt as _};
use serde::{Serialize, de::DeserializeOwned};
use shoal_protocol::hub::link::Sealed;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinHandle},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

use super::sealing::{Keys, Opener, Sealer};

/// The largest frame either side takes.
const MAX_FRAME: usize = 16 << 20;

/// Frames waiting to be sent before a sender waits.
const QUEUED: usize = 64;

/// How long closing a channel waits for its queued frames to go.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One side of an open transport.
pub(super) struct Channel {
    incoming: mpsc::Receiver<Result<String>>,
    outgoing: mpsc::Sender<Out>,
    /// Opens the frames received once the channel is sealed.
    opener: Option<Opener>,
    /// Whether the last frame received came sealed, which only the other
    /// side of the key exchange can send.
    sealed_last: bool,
    _reader: Reader,
    writer: JoinHandle<()>,
}

/// What the writer takes, in the order senders queued it.
enum Out {
    /// A frame's text, with whom to tell once it is written.
    Frame {
        text: String,
        written: Option<oneshot::Sender<()>>,
    },
    /// Seal every later frame with this.
    Seal(Box<Sealer>),
}

/// The task that reads a channel's frames, which ends with it.
struct Reader(AbortHandle);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What sends frames on a [`Channel`], which tasks other than its reader
/// hold.
#[derive(Clone)]
pub(super) struct Sender(mpsc::Sender<Out>);

impl Sender {
    pub(super) async fn send(&self, frame: &impl Serialize) -> Result<()> {
        self.queue(frame, None).await
    }

    /// Send `frame` and return once it is written to the transport.
    pub(super) async fn send_written(&self, frame: &impl Serialize) -> Result<()> {
        let (written, wrote) = oneshot::channel();
        self.queue(frame, Some(written)).await?;
        wrote
            .await
            .map_err(|_| anyhow!("the link closed before the frame went"))
    }

    async fn queue(
        &self,
        frame: &impl Serialize,
        written: Option<oneshot::Sender<()>>,
    ) -> Result<()> {
        let text = serde_json::to_string(frame)?;
        let out = Out::Frame { text, written };
        (self.0.send(out).await).map_err(|_| anyhow!("the link closed"))
    }
}

impl Channel {
    /// A channel over `reader` and `writer`, a frame per line.
    pub(super) fn lines(
        reader: impl AsyncBufRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        let (incoming, received) = mpsc::channel(QUEUED);
        let (outgoing, to_send) = mpsc::channel(QUEUED);
        Self {
            incoming: received,
            outgoing,
            opener: None,
            sealed_last: false,
            _reader: Reader(tokio::spawn(read_lines(reader, incoming)).abort_handle()),
            writer: tokio::spawn(write_frames(Lines(writer), to_send)),
        }
    }

    /// A channel over `socket`, a frame per text message.
    pub(super) fn websocket<S>(socket: WebSocketStream<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        let (sink, mut stream) = socket.split();
        let (incoming, received) = mpsc::channel(QUEUED);
        let (outgoing, to_send) = mpsc::channel(QUEUED);
        let reader = tokio::spawn(async move {
            loop {
                let message = match stream.next().await {
                    None | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(Message::Text(text))) => Ok(text.as_str().to_owned()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Binary(_))) => Err(anyhow!("a binary message")),
                    Some(Err(error)) => Err(error.into()),
                };
                let failed = message.is_err();
                if incoming.send(message).await.is_err() || failed {
                    return;
                }
            }
        });
        Self {
            incoming: received,
            outgoing,
            opener: None,
            sealed_last: false,
            _reader: Reader(reader.abort_handle()),
            writer: tokio::spawn(write_frames(Messages(sink), to_send)),
        }
    }

    pub(super) fn sender(&self) -> Sender {
        Sender(self.outgoing.clone())
    }

    pub(super) async fn send(&self, frame: &impl Serialize) -> Result<()> {
        self.sender().send(frame).await
    }

    /// Seal the frames sent after those already queued, and open those
    /// received from the next one on.
    pub(super) async fn seal(&mut self, keys: Keys) -> Result<()> {
        let Keys { sending, receiving } = keys;
        self.opener = Some(receiving);
        (self.outgoing.send(Out::Seal(Box::new(sending))).await)
            .map_err(|_| anyhow!("the link closed"))
    }

    /// Send `frame`, then close the channel once it went, or the other side
    /// stopped reading.
    pub(super) async fn close_with(self, frame: &impl Serialize) {
        let _ = self.send(frame).await;
        let Self {
            outgoing, writer, ..
        } = self;
        drop(outgoing);
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, writer).await;
    }

    /// The next frame, none once the other side closed the link.
    pub(super) async fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        let Some(text) = self.incoming.recv().await else {
            return Ok(None);
        };
        let text = self.open(text?)?;
        let frame = serde_json::from_str(&text)
            .with_context(|| format!("unreadable hub link frame: {}", excerpt(&text)))?;
        Ok(Some(frame))
    }

    /// Whether the last frame received came sealed, so that the other side
    /// of the key exchange sent it.
    pub(super) fn sealed_last(&self) -> bool {
        self.sealed_last
    }

    /// The frame `text` carries, opened once the channel is sealed. A
    /// refusal before the first sealed frame comes as it is: the hub refuses
    /// some links before it can seal them. Anyone between the sides could
    /// send one, so [`Self::sealed_last`] tells it apart.
    fn open(&mut self, text: String) -> Result<String> {
        self.sealed_last = false;
        let Some(opener) = &mut self.opener else {
            return Ok(text);
        };
        match serde_json::from_str::<Sealed>(&text) {
            Ok(sealed) => {
                let frame = opener.open(sealed)?;
                self.sealed_last = true;
                Ok(frame)
            }
            Err(_) if opener.fresh() && is_refusal(&text) => Ok(text),
            Err(error) => Err(anyhow!(error).context(format!(
                "an unsealed frame on a sealed hub link: {}",
                excerpt(&text)
            ))),
        }
    }

    /// The next frame, which must come.
    pub(super) async fn expect<T: DeserializeOwned>(&mut self) -> Result<T> {
        match self.receive().await? {
            Some(frame) => Ok(frame),
            None => bail!("the link closed"),
        }
    }
}

/// Whether `text` is a refusal frame.
fn is_refusal(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .is_ok_and(|frame| frame.get("refused").is_some())
}

/// Where a writer puts a frame's text.
trait Wire {
    /// Write `text` whole, failing once the transport is gone.
    async fn write(&mut self, text: String) -> Result<()>;
    async fn close(&mut self);
}

/// A byte stream that takes a frame per line.
struct Lines<W>(W);

impl<W: AsyncWrite + Send + Unpin> Wire for Lines<W> {
    async fn write(&mut self, mut text: String) -> Result<()> {
        text.push('\n');
        self.0.write_all(text.as_bytes()).await?;
        Ok(self.0.flush().await?)
    }

    async fn close(&mut self) {
        let _ = self.0.shutdown().await;
    }
}

/// A WebSocket that takes a frame per text message.
struct Messages<S>(S);

impl<S> Wire for Messages<S>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Send + Unpin,
{
    async fn write(&mut self, text: String) -> Result<()> {
        Ok(self.0.send(Message::text(text)).await?)
    }

    async fn close(&mut self) {
        let _ = self.0.close().await;
    }
}

/// Write each frame `to_send` passes to `wire`, sealed once it passes a
/// sealer, telling each sender that waits once its frame is written, until
/// the channel closes or a write fails.
async fn write_frames(mut wire: impl Wire, mut to_send: mpsc::Receiver<Out>) {
    let mut sealer = None;
    while let Some(out) = to_send.recv().await {
        let (text, written) = match out {
            Out::Seal(next) => {
                sealer = Some(*next);
                continue;
            }
            Out::Frame { text, written } => (text, written),
        };
        let text = match &mut sealer {
            Some(sealer) => match serde_json::to_string(&sealer.seal(text)) {
                Ok(text) => text,
                Err(_) => return,
            },
            None => text,
        };
        if wire.write(text).await.is_err() {
            return;
        }
        if let Some(written) = written {
            let _ = written.send(());
        }
    }
    wire.close().await;
}

/// The start of `text`, enough to tell what it is.
fn excerpt(text: &str) -> &str {
    let end = (text.char_indices().map(|(at, _)| at))
        .nth(200)
        .unwrap_or(text.len());
    &text[..end]
}

/// Pass each line of `reader` to `incoming`, until it ends or fails.
async fn read_lines(
    mut reader: impl AsyncBufRead + Send + Unpin,
    incoming: mpsc::Sender<Result<String>>,
) {
    loop {
        let line = read_line(&mut reader).await;
        let ended = !matches!(line, Ok(Some(_)));
        let Some(line) = line.transpose() else {
            return;
        };
        if incoming.send(line).await.is_err() || ended {
            return;
        }
    }
}

/// The next line of `reader`, none at its end.
async fn read_line(reader: &mut (impl AsyncBufRead + Send + Unpin)) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    (reader.take(MAX_FRAME as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.is_empty() {
        return Ok(None);
    }
    ensure!(
        bytes.len() <= MAX_FRAME,
        "a hub link frame exceeds {MAX_FRAME} bytes"
    );
    ensure!(
        bytes.last() == Some(&b'\n'),
        "the link closed partway through a frame"
    );
    bytes.pop();
    Ok(Some(String::from_utf8(bytes)?))
}
