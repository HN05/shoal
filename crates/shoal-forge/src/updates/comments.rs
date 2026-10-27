//! The comments and reviews a snapshot records, and whether they changed.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A comment or review as a snapshot records it: what changes when it is
/// edited, dismissed or answered, and who wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct Entry {
    pub(crate) fingerprint: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) author: String,
}

/// An entry as saved, including the bare fingerprint of snapshots saved
/// before authors were recorded.
#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Fingerprint(String),
    Entry {
        fingerprint: String,
        #[serde(default)]
        author: String,
    },
}

impl From<Stored> for Entry {
    fn from(stored: Stored) -> Self {
        match stored {
            Stored::Fingerprint(fingerprint) => Self::from(fingerprint),
            Stored::Entry {
                fingerprint,
                author,
            } => Self {
                fingerprint,
                author,
            },
        }
    }
}

/// An entry whose author is unknown.
impl From<String> for Entry {
    fn from(fingerprint: String) -> Self {
        Self {
            fingerprint,
            author: String::new(),
        }
    }
}

impl From<&str> for Entry {
    fn from(fingerprint: &str) -> Self {
        Self::from(fingerprint.to_owned())
    }
}

impl Entry {
    pub(crate) fn new(fingerprint: String, author: String) -> Self {
        Self {
            fingerprint,
            author,
        }
    }

    /// Whether the entry records anything; an empty one reads as absent.
    fn recorded(&self) -> bool {
        !self.fingerprint.is_empty()
    }
}

/// Whether `next` adds, edits or deletes a comment or review since
/// `previous`.
pub(crate) fn changed(previous: &BTreeMap<String, Entry>, next: &BTreeMap<String, Entry>) -> bool {
    let added_or_edited = next
        .iter()
        .filter(|(_, entry)| entry.recorded())
        .any(|(key, entry)| {
            recorded(previous, key).map(|seen| &seen.fingerprint) != Some(&entry.fingerprint)
        });
    let deleted = previous
        .iter()
        .filter(|(_, entry)| entry.recorded())
        .any(|(key, _)| recorded(next, key).is_none());
    added_or_edited || deleted
}

/// The entry under `key`, unless it records nothing.
fn recorded<'a>(entries: &'a BTreeMap<String, Entry>, key: &str) -> Option<&'a Entry> {
    entries.get(key).filter(|entry| entry.recorded())
}
