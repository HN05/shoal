//! Reading and writing frames, and splitting what does not fit in one.
use super::Response;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub const MAX_FRAME: usize = 64 * 1024;

pub async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    read_buffered(&mut BufReader::new(stream)).await
}

pub async fn read_buffered<T: DeserializeOwned>(
    stream: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<T> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "protocol frame exceeds {MAX_FRAME} bytes"
    );
    // The peer closed the connection, possibly partway through a frame.
    if bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "incomplete protocol frame",
        )
        .into());
    }
    serde_json::from_slice(&bytes).context("invalid protocol message")
}

/// A response, joining the pages of one sent in several frames.
pub async fn read_response(
    stream: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<Response> {
    let mut response: Response = read_buffered(stream).await?;
    while response.more {
        let page: Response = read_buffered(stream).await?;
        ensure!(
            page.id == response.id,
            "a response page answers another request"
        );
        response.body.extend(page.body)?;
        response.more = page.more;
    }
    Ok(response)
}

/// Responses read one after another from a stream, such as a subscription's,
/// in a way that loses nothing when a read is cancelled: the start of a frame
/// and the earlier pages of a response wait for the next read.
pub struct Responses<R> {
    reader: R,
    /// Bytes read past the last whole frame.
    buffer: Vec<u8>,
    /// A paged response whose remaining pages have yet to arrive.
    joined: Option<Response>,
}

impl<R: AsyncRead + Unpin> Responses<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            joined: None,
        }
    }

    /// The next response, joining the pages of one sent in several frames.
    pub async fn next(&mut self) -> Result<Response> {
        loop {
            let page: Response = self.frame().await?;
            let Some(mut response) = self.joined.take() else {
                if page.more {
                    self.joined = Some(page);
                    continue;
                }
                return Ok(page);
            };
            ensure!(
                page.id == response.id,
                "a response page answers another request"
            );
            response.body.extend(page.body)?;
            response.more = page.more;
            if !response.more {
                return Ok(response);
            }
            self.joined = Some(response);
        }
    }

    /// The next frame. A read fills the buffer only once it returns, so a
    /// cancelled one leaves it as it was.
    async fn frame<T: DeserializeOwned>(&mut self) -> Result<T> {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let frame: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&frame).context("invalid protocol message");
            }
            ensure!(
                self.buffer.len() <= MAX_FRAME,
                "protocol frame exceeds {MAX_FRAME} bytes"
            );
            let mut chunk = [0u8; 8192];
            let read = self.reader.read(&mut chunk).await?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "incomplete protocol frame",
                )
                .into());
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// `items` in pages that each fit in a frame with room for the envelope,
/// in order. There is always a first page, empty when `items` is.
pub fn frame_pages<T: Serialize>(items: Vec<T>) -> Result<Vec<Vec<T>>> {
    const PAGE_BYTES: usize = MAX_FRAME - 1024;
    let mut pages = vec![Vec::new()];
    let mut bytes = 0;
    for item in items {
        let size = serde_json::to_vec(&item)?.len() + 1;
        let page = pages.last_mut().expect("pages start with one");
        if bytes + size > PAGE_BYTES && !page.is_empty() {
            pages.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        pages.last_mut().expect("pages start with one").push(item);
    }
    Ok(pages)
}

/// `text` in pieces that each fit in a frame's page, however much JSON
/// escaping grows them.
pub fn text_pieces(text: &str) -> Vec<String> {
    const PIECE_BYTES: usize = MAX_FRAME / 4;
    let mut pieces = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(PIECE_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        pieces.push(piece.to_owned());
        rest = tail;
    }
    pieces
}

/// A message longer than [`MAX_FRAME`]; none of it was written.
#[derive(Debug)]
pub struct FrameTooLarge {
    pub bytes: usize,
}

impl std::fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the {}-byte message exceeds the {MAX_FRAME}-byte protocol frame",
            self.bytes
        )
    }
}

impl std::error::Error for FrameTooLarge {}

pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME {
        return Err(FrameTooLarge { bytes: bytes.len() }.into());
    }
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
