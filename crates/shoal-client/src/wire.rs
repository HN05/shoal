//! The frames on a client API connection: newline-delimited JSON, each at
//! most [`MAX_FRAME`] bytes. The client opens with [`Opening`], the daemon
//! answers with a [`HandshakeReply`], then each [`Request`] gets
//! [`Response`]s with its ID until its result or error.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    errors::{ApiError, Error},
    methods::Call,
    records::{Record, RecordKey},
};

/// The largest frame either side sends or reads.
pub const MAX_FRAME: usize = 1024 * 1024;
/// The size the daemon keeps a page of a listing or snapshot within.
pub const PAGE_BYTES: usize = 64 * 1024;

/// The client's first frame. Its `client_api` key tells the daemon the
/// connection speaks this API rather than Shoal's internal protocol.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Opening {
    pub client_api: Hello,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Hello {
    /// The API version the client speaks, such as `1.0.0`.
    pub version: String,
    /// A workspace's scope token; without one the caller is the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The client's name, for the daemon's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// The daemon's answer to [`Opening`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeReply {
    Welcome(Welcome),
    /// The daemon refused the connection, such as with `unsupported_version`.
    Error(ApiError),
}

/// What the daemon serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Welcome {
    /// The API version the daemon speaks.
    pub version: String,
    /// The major versions the daemon serves.
    pub supported_majors: Vec<u64>,
    /// The methods and subscription topics the daemon offers, such as
    /// `workspace.get` and `topic:workspaces`.
    pub features: Vec<String>,
}

impl Welcome {
    /// Whether the daemon offers `feature`.
    pub fn offers(&self, feature: &str) -> bool {
        self.features.iter().any(|offered| offered == feature)
    }
}

/// A request; its responses carry its ID.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    pub id: u64,
    #[serde(flatten)]
    pub reply: Reply,
}

/// One response to a request. A result too large for one frame arrives as
/// `page` frames holding the first items of its list, then a `result`
/// holding the rest.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Page(Vec<serde_json::Value>),
    Result(serde_json::Value),
    Error(ApiError),
    /// A subscription's records, until the client hangs up.
    Event(Event),
}

/// What a subscription sends: its snapshot in one or more parts, then each
/// change. After a daemon restart the connection closes; a new subscription
/// starts with a new snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Some of the records the subscription covers, as they are now;
    /// `complete` marks the last part.
    Snapshot {
        records: Vec<Record>,
        complete: bool,
    },
    /// A record was added or changed, and is now `record`.
    Changed { record: Record },
    /// The record `key` names was removed.
    Removed { key: RecordKey },
    /// An event this version does not know; ignore it.
    #[serde(other)]
    Unknown,
}

/// Read one frame, or `None` once the peer closed the connection between frames.
pub async fn read<T: DeserializeOwned>(
    stream: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<T>, Error> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_FRAME {
        return Err(Error::Protocol(format!(
            "a frame exceeds {MAX_FRAME} bytes"
        )));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(Error::Closed);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| Error::Protocol(error.to_string()))
}

/// Write one frame, refusing one larger than [`MAX_FRAME`] before writing any of it.
pub async fn write<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<(), Error> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| Error::Protocol(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME {
        return Err(Error::Protocol(format!(
            "the {}-byte frame exceeds {MAX_FRAME} bytes",
            bytes.len()
        )));
    }
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// `items` in pages that each stay within [`PAGE_BYTES`], in order. There is
/// always a last page, empty when `items` is.
pub fn pages<T: Serialize>(items: Vec<T>) -> Result<Vec<Vec<T>>, serde_json::Error> {
    let mut pages = vec![Vec::new()];
    let mut bytes = 0;
    for item in items {
        let size = serde_json::to_vec(&item)?.len() + 1;
        if bytes + size > PAGE_BYTES && pages.last().is_some_and(|page| !page.is_empty()) {
            pages.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        if let Some(page) = pages.last_mut() {
            page.push(item);
        }
    }
    Ok(pages)
}

/// The major version of `version`, such as 1 for `1.2.0`.
pub fn major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests;
