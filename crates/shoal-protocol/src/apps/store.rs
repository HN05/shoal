//! The app store: entries of bytes that each app keeps under keys of its
//! own. Shoal never reads an entry's bytes. A write or deletion names the
//! revision it replaces, so a stale one is refused instead of overwriting
//! another change.
use std::{fmt, str::FromStr};

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use shoal_states::{states, text_key};

use crate::{protocol::MAX_FRAME, time::UnixTime};

states! {
    /// Whom an entry belongs to.
    AppScope {
        /// Every machine and device of the user, through the hub when
        /// there is one.
        Shared => "shared",
        /// The machine or device that writes it.
        Device => "device",
    }
}

/// The key of an entry: `/`-separated segments, such as `layout/main`,
/// none empty, `.` or `..`, of printable text, at most [`EntryKey::MAX`]
/// bytes in all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryKey(String);

impl EntryKey {
    pub const MAX: usize = 512;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for EntryKey {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(
            !text.is_empty() && text.len() <= Self::MAX,
            "an entry's key takes 1 to {} bytes",
            Self::MAX
        );
        ensure!(
            !text.contains(char::is_control),
            "an entry's key has no control characters: {text:?}"
        );
        ensure!(
            text.split('/')
                .all(|segment| !matches!(segment, "" | "." | "..")),
            "an entry's key is segments separated by single slashes, none . or ..: {text:?}"
        );
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for EntryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(EntryKey);

/// The start of the keys a listing or a watch covers, such as `exports/`;
/// empty for every key. Printable text of at most [`EntryKey::MAX`] bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct KeyPrefix(String);

impl KeyPrefix {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn covers(&self, key: &EntryKey) -> bool {
        key.as_str().starts_with(&self.0)
    }
}

impl FromStr for KeyPrefix {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= EntryKey::MAX,
            "a key prefix takes at most {} bytes",
            EntryKey::MAX
        );
        ensure!(
            !text.contains(char::is_control),
            "a key prefix has no control characters: {text:?}"
        );
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for KeyPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(KeyPrefix);

/// What an entry's bytes are, such as `application/json`, which is what an
/// app's settings take: printable ASCII of at most
/// [`ContentType::MAX`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentType(String);

impl ContentType {
    pub const MAX: usize = 255;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ContentType {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(
            !text.is_empty() && text.len() <= Self::MAX,
            "a content type takes 1 to {} bytes",
            Self::MAX
        );
        ensure!(
            text.bytes().all(|byte| (b' '..=b'~').contains(&byte)),
            "a content type is printable ASCII: {text:?}"
        );
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

text_key!(ContentType);

/// An entry's revision: each change of the daemon's entries takes the next
/// one, so a later change has a higher revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// What an entry is, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub key: EntryKey,
    pub content_type: ContentType,
    pub revision: Revision,
    /// How many bytes it holds.
    pub size: u64,
    pub updated_at: UnixTime,
}

/// An entry with its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub entry: Entry,
    pub bytes: Vec<u8>,
}

/// What to write to an entry: what its bytes are, and the revision it
/// replaces, none for a key that holds no entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    pub key: EntryKey,
    pub content_type: ContentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Revision>,
}

/// The most bytes of an entry one piece carries, so that a piece in base64
/// fits a frame.
const PIECE_BYTES: usize = MAX_FRAME / 4 * 3 / 2;

/// A piece of an entry as a read sends it, in order: the entry, then its
/// bytes in base64 pieces that each fit a frame.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPiece {
    Entry(Entry),
    Bytes(String),
}

impl EntryPiece {
    /// The pieces that carry `stored`.
    pub fn split(stored: Stored) -> Vec<Self> {
        let bytes =
            (stored.bytes.chunks(PIECE_BYTES)).map(|chunk| Self::Bytes(BASE64.encode(chunk)));
        std::iter::once(Self::Entry(stored.entry))
            .chain(bytes)
            .collect()
    }

    /// The entry `pieces` carry, none for no pieces.
    pub fn join(pieces: Vec<Self>) -> Result<Option<Stored>> {
        let mut pieces = pieces.into_iter();
        let Some(first) = pieces.next() else {
            return Ok(None);
        };
        let Self::Entry(entry) = first else {
            anyhow::bail!("an entry's pieces start with the entry");
        };
        let mut bytes = Vec::new();
        for piece in pieces {
            let Self::Bytes(piece) = piece else {
                anyhow::bail!("an entry's pieces hold one entry");
            };
            (bytes.extend(
                BASE64
                    .decode(piece)
                    .context("an invalid piece of an entry")?,
            ));
        }
        ensure!(
            bytes.len() as u64 == entry.size,
            "the entry {} holds {} bytes, not the {} it came with",
            entry.key,
            entry.size,
            bytes.len()
        );
        Ok(Some(Stored { entry, bytes }))
    }
}

/// A piece of the bytes a write sends after the daemon took its request,
/// in base64, at most [`AppData::BYTES`] bytes each.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppData(pub String);

impl AppData {
    pub const BYTES: usize = PIECE_BYTES;

    /// The pieces that carry `bytes`.
    pub fn split(bytes: &[u8]) -> impl Iterator<Item = Self> + '_ {
        (bytes.chunks(Self::BYTES)).map(|chunk| Self(BASE64.encode(chunk)))
    }

    /// The bytes it carries.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = BASE64
            .decode(&self.0)
            .context("an invalid piece of a write")?;
        ensure!(
            bytes.len() <= Self::BYTES,
            "a piece of a write holds at most {} bytes",
            Self::BYTES
        );
        Ok(bytes)
    }
}

/// A change a watch reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// The entry was written.
    Written(Entry),
    /// The entry under the key was deleted.
    Deleted(EntryKey),
}

#[cfg(test)]
mod tests;
