//! Messages of a link too large for one frame, which go as
//! [`Frame::Piece`]s.
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, ensure};
use shoal_protocol::hub::link::Frame;

use crate::transport::Sender;

/// The most JSON one piece carries, so that a piece, escaped as a string
/// and then sealed, stays well within a frame.
const PIECE_BYTES: usize = 4 << 20;

/// The most a message put back together from pieces may hold.
const MESSAGE_BYTES: usize = 256 << 20;

/// The number of the next message sent in pieces.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Send `frame`, in pieces when it is too large for one, and with
/// `written` return once its last frame is written.
pub async fn send(sender: &Sender, frame: &Frame, written: bool) -> Result<()> {
    let frames = split(frame, PIECE_BYTES)?;
    let last = frames.len() - 1;
    for (index, frame) in frames.iter().enumerate() {
        match written && index == last {
            true => sender.send_written(frame).await?,
            false => sender.send(frame).await?,
        }
    }
    Ok(())
}

/// The frames that carry `frame`: itself, or pieces of its JSON of at most
/// `bytes` each.
fn split(frame: &Frame, bytes: usize) -> Result<Vec<Frame>> {
    let text = serde_json::to_string(frame).context("write a hub link frame")?;
    if text.len() <= bytes {
        return Ok(vec![frame.clone()]);
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut pieces = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let mut end = bytes.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (data, after) = rest.split_at(end);
        rest = after;
        pieces.push(Frame::Piece {
            id,
            data: data.to_owned(),
            more: !rest.is_empty(),
        });
    }
    Ok(pieces)
}

/// The messages the other side is sending in pieces, as far as they came.
#[derive(Default)]
pub struct Assembly(HashMap<u64, String>);

impl Assembly {
    /// Take the piece `data` of the message `id`, and the message once it is
    /// whole: the last piece came, `more` false.
    pub fn take(&mut self, id: u64, data: String, more: bool) -> Result<Option<Frame>> {
        let message = self.0.entry(id).or_default();
        message.push_str(&data);
        ensure!(
            message.len() <= MESSAGE_BYTES,
            "a hub link message outgrew {MESSAGE_BYTES} bytes"
        );
        if more {
            return Ok(None);
        }
        let message = self.0.remove(&id).unwrap_or_default();
        let frame = serde_json::from_str(&message).context("read a hub link message")?;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests;
