//! Sending a file to another joined machine's daemon over a duplex stream
//! of the hub's links, once the request that daemon answers asks for it,
//! and reading its answer to the file from the same stream.
use std::path::Path;

use anyhow::{Result, bail};
use shoal_protocol::protocol::{Body, Response};

use super::{Incoming, Outgoing};

/// Send the file `file` on `outgoing` once the other machine's daemon,
/// which writes its answers on `incoming`, asked for it, and return its
/// answer to the file: one that comes before the whole file went, as from
/// a daemon that refuses it, ends the sending.
pub(crate) async fn deliver_file(
    file: &Path,
    incoming: Incoming,
    outgoing: Outgoing,
) -> Result<Body> {
    let mut answers = Answers::new(incoming);
    answers.next().await?;
    let sending = async move {
        let mut outgoing = outgoing;
        if outgoing.write_file(file).await.is_ok() {
            outgoing.end(None).await;
        }
    };
    let answer = answers.next();
    tokio::pin!(answer);
    tokio::select! {
        answer = &mut answer => return answer,
        () = sending => {}
    }
    answer.await
}

/// What `work`, which blocks on the file system, returns, run on the
/// blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    Ok(tokio::task::spawn_blocking(work).await??)
}

/// The answers another machine's daemon writes on a stream, frame by frame.
struct Answers {
    incoming: Incoming,
    read: Vec<u8>,
}

impl Answers {
    fn new(incoming: Incoming) -> Self {
        Self {
            incoming,
            read: Vec::new(),
        }
    }

    /// The next answer's body, past any the daemon sent while it still
    /// prepared it; its refusal as an error. Cancelling it loses nothing.
    async fn next(&mut self) -> Result<Body> {
        loop {
            if let Some(end) = self.read.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.read.drain(..=end).collect();
                let response: Response = serde_json::from_slice(&line)?;
                if !matches!(response.body, Body::Keepalive) {
                    return response.body.into_result();
                }
                continue;
            }
            match self.incoming.next().await? {
                Some(bytes) => self.read.extend(bytes),
                None => bail!("the other machine's daemon ended the stream unanswered"),
            }
        }
    }
}
