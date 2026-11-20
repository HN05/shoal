//! Attaching a terminal on one machine to an agent's session on another:
//! the owning machine runs Shoal's own attach on a terminal it makes for
//! the viewer, and the connection carries that terminal's screen one way
//! and the viewer's keys and size the other, each as frames of a tag, a
//! length and the bytes.
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _};

use crate::sessions::{HostName, Origin};

/// The most bytes a frame carries.
pub const MAX_FRAME: usize = 1 << 20;

/// The size of a terminal, in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub rows: u16,
    pub columns: u16,
}

/// The terminal on another machine, or on a device, that an attach shows a
/// session in, and what it asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewer {
    /// The session to attach, else the one `shoal attach` chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The place of the session to attach to again after a lost
    /// connection, without waking or restoring the agent a second time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub again: Option<Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
    /// The viewer's `TERM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
    /// The viewer's apps that connect to a host's servers on several
    /// machines and show their sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<ViewerApp>,
}

/// An app of the viewer's that shows the sessions of a host's servers on
/// several machines, connecting to each as a client of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerApp {
    pub host: HostName,
    /// The label the app reports to each server.
    pub label: String,
}

impl Viewer {
    /// The label of the viewer's app for `host`, when it has one.
    pub fn app(&self, host: HostName) -> Option<&str> {
        let app = self.apps.iter().find(|app| app.host == host);
        app.map(|app| app.label.as_str())
    }
}

/// The session an attach chose, and where its host keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenSession {
    pub name: String,
    pub place: Origin,
}

/// What an attach run for a viewer on another machine reports to the
/// daemon that runs it, a line each: the session it chose, then whether
/// that session still runs once the viewer left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachReport {
    Chosen(ChosenSession),
    Left { open: bool },
}

/// What the viewer sends once the owning machine answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToSession {
    /// Keys typed, as the terminal sends them.
    Keys(Vec<u8>),
    /// The viewer's terminal took a new size.
    Resize(TerminalSize),
}

/// What the owning machine sends once it answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromSession {
    /// Bytes the attach wrote to its terminal.
    Screen(Vec<u8>),
    /// The session the attach chose, whose place attaching again names.
    Session(ChosenSession),
    /// The attach left its session, which still runs when `true`.
    Left(bool),
    /// The attach ended with this exit code; the last frame.
    Exited(i32),
}

const KEYS: u8 = 1;
const RESIZE: u8 = 2;
const SCREEN: u8 = 3;
const SESSION: u8 = 4;
const EXITED: u8 = 5;
const LEFT: u8 = 6;

impl ToSession {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Keys(keys) => frame(KEYS, keys),
            Self::Resize(size) => frame(RESIZE, &size_bytes(*size)),
        }
    }

    /// The next frame on `reader`; none once it ends between frames.
    pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<Self>> {
        let Some((tag, bytes)) = read_frame(reader).await? else {
            return Ok(None);
        };
        Ok(Some(match tag {
            KEYS => Self::Keys(bytes),
            RESIZE => Self::Resize(size_of(&bytes)?),
            tag => bail!("unknown attach frame {tag}"),
        }))
    }
}

impl FromSession {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Screen(bytes) => frame(SCREEN, bytes),
            Self::Session(chosen) => frame(
                SESSION,
                &serde_json::to_vec(chosen).expect("a session serializes"),
            ),
            Self::Left(open) => frame(LEFT, &[u8::from(*open)]),
            Self::Exited(code) => frame(EXITED, &code.to_be_bytes()),
        }
    }

    /// The next frame on `reader`; none once it ends between frames.
    pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<Self>> {
        let Some((tag, bytes)) = read_frame(reader).await? else {
            return Ok(None);
        };
        Ok(Some(match tag {
            SCREEN => Self::Screen(bytes),
            SESSION => Self::Session(serde_json::from_slice(&bytes).context("a chosen session")?),
            LEFT => match bytes.as_slice() {
                [open] => Self::Left(*open != 0),
                _ => bail!("a left session's frame holds one byte"),
            },
            EXITED => {
                let code = <[u8; 4]>::try_from(bytes.as_slice()).context("an exit code")?;
                Self::Exited(i32::from_be_bytes(code))
            }
            tag => bail!("unknown attach frame {tag}"),
        }))
    }
}

fn frame(tag: u8, bytes: &[u8]) -> Vec<u8> {
    let length = u32::try_from(bytes.len()).expect("a frame fits in MAX_FRAME");
    let mut frame = Vec::with_capacity(5 + bytes.len());
    frame.push(tag);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(bytes);
    frame
}

/// A frame's tag and bytes; none when `reader` ends before its tag.
async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0; 1];
    if reader.read(&mut tag).await? == 0 {
        return Ok(None);
    }
    let mut length = [0; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(length <= MAX_FRAME, "an attach frame of {length} bytes");
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(Some((tag[0], bytes)))
}

fn size_bytes(size: TerminalSize) -> [u8; 4] {
    let [r0, r1] = size.rows.to_be_bytes();
    let [c0, c1] = size.columns.to_be_bytes();
    [r0, r1, c0, c1]
}

fn size_of(bytes: &[u8]) -> Result<TerminalSize> {
    let [r0, r1, c0, c1] = <[u8; 4]>::try_from(bytes).context("a terminal size")?;
    Ok(TerminalSize {
        rows: u16::from_be_bytes([r0, r1]),
        columns: u16::from_be_bytes([c0, c1]),
    })
}

#[cfg(test)]
mod tests;
