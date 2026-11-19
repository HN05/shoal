//! A workspace's diff as its own machine's Git writes it, for a caller on
//! another machine, whose own Git decides its colour and pager.
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How the caller shows a streamed diff, as its own Git would show it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffDisplay {
    /// Whether Git colours the diff.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub color: bool,
    /// The width of the caller's terminal, which Git and an external diff
    /// read as `COLUMNS`; none off a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u16>,
}

/// A frame of a streamed diff: what Git wrote, then how it exited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffOutput {
    /// Bytes Git wrote to its standard output, in standard base64.
    Stdout(String),
    /// Bytes Git wrote to its standard error, in standard base64.
    Stderr(String),
    /// Git's exit code, the stream's last frame.
    Exited(i32),
}

impl DiffOutput {
    pub fn stdout(bytes: &[u8]) -> Self {
        Self::Stdout(encode(bytes))
    }

    pub fn stderr(bytes: &[u8]) -> Self {
        Self::Stderr(encode(bytes))
    }
}

/// The bytes a frame of Git's output carries.
pub fn decode(data: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
