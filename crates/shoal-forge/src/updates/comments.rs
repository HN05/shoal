//! The comments and reviews a snapshot records, and the summary of what
//! changed among them.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    New,
    Edited,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Comment,
    InlineComment,
    Review,
}

impl Kind {
    /// The kind of the entry under `key`, named after its source.
    fn of(key: &str) -> Self {
        match key.split_once(':').map(|(source, _)| source) {
            Some("reviews") => Self::Review,
            Some("inline") => Self::InlineComment,
            _ => Self::Comment,
        }
    }

    fn noun(self, count: usize) -> &'static str {
        match (self, count) {
            (Self::Comment, 1) => "comment",
            (Self::Comment, _) => "comments",
            (Self::InlineComment, 1) => "inline comment",
            (Self::InlineComment, _) => "inline comments",
            (Self::Review, 1) => "review",
            (Self::Review, _) => "reviews",
        }
    }

    /// The word for `change` to an entry of this kind. A review also
    /// changes through replies, dismissal or new commits, not only edits.
    fn change(self, change: Change) -> &'static str {
        match (change, self) {
            (Change::New, _) => "new",
            (Change::Edited, Self::Review) => "updated",
            (Change::Edited, _) => "edited",
            (Change::Deleted, _) => "deleted",
        }
    }
}

/// The comments and reviews `next` adds, edits or deletes since
/// `previous`, counted by change, kind and author, such as `2 new comments
/// from alice, 1 updated review from bot`; `None` when nothing changed.
pub(crate) fn summary(
    previous: &BTreeMap<String, Entry>,
    next: &BTreeMap<String, Entry>,
) -> Option<String> {
    let mut counts = BTreeMap::<(Change, Kind, &str), usize>::new();
    for (key, entry) in next.iter().filter(|(_, entry)| entry.recorded()) {
        let change = match recorded(previous, key) {
            None => Change::New,
            Some(seen) if seen.fingerprint != entry.fingerprint => Change::Edited,
            Some(_) => continue,
        };
        *counts
            .entry((change, Kind::of(key), &entry.author))
            .or_default() += 1;
    }
    for (key, entry) in previous.iter().filter(|(_, entry)| entry.recorded()) {
        if recorded(next, key).is_none() {
            *counts
                .entry((Change::Deleted, Kind::of(key), &entry.author))
                .or_default() += 1;
        }
    }
    (!counts.is_empty()).then(|| {
        counts
            .into_iter()
            .map(|((change, kind, author), count)| {
                let words = format!("{count} {} {}", kind.change(change), kind.noun(count));
                if author.is_empty() {
                    words
                } else {
                    format!("{words} from {author}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// The entry under `key`, unless it records nothing.
fn recorded<'a>(entries: &'a BTreeMap<String, Entry>, key: &str) -> Option<&'a Entry> {
    entries.get(key).filter(|entry| entry.recorded())
}
