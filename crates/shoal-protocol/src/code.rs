//! Code references: places in a worktree that a post in a thread points
//! at, and how they follow the code as it changes.
use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;
use shoal_states::states;

use crate::threads::ThreadSpace;

mod lines;

pub use lines::{Followed, Lines, follow};

/// Longest snippet a reference keeps of its lines, in bytes, which keeps a
/// thread with several to one response.
pub const MAX_SNIPPET_LEN: usize = 2048;
/// Largest file whose versions and snippet a reference keeps.
pub const MAX_VERSIONED_LEN: usize = 1024 * 1024;

states!(
    /// What became of a reference's code since it was made.
    CodeState {
        /// Nothing inside it changed; it may have moved.
        Unchanged => "unchanged",
        /// Lines inside it were edited, and it covers what replaced them.
        Changed => "changed",
        /// Its code is gone, deleted or moved to another file; it keeps the
        /// point where the code was and its snippet.
        Outdated => "outdated",
    }
);

impl CodeState {
    /// The state after `self` once a later step found `next`: a reference
    /// once changed or outdated stays so.
    pub fn then(self, next: Self) -> Self {
        let rank = |state| match state {
            Self::Unchanged => 0,
            Self::Changed => 1,
            Self::Outdated => 2,
        };
        if rank(next) > rank(self) { next } else { self }
    }
}

/// A place in a worktree that a new post points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCodeRef {
    /// A file or directory, by absolute path, in the worktree of a space
    /// the caller's scope reaches, which the reference names.
    pub path: PathBuf,
    /// Its lines; none points at the whole file or directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Lines>,
    /// The file as an editor's buffer holds it; without, the file on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
}

/// Where a reference's code is in one version of its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAt {
    /// The file or directory, relative to the worktree's root.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Lines>,
    /// The Git blob of the file's version, which Shoal keeps; none for a
    /// directory, a binary file or one over [`MAX_VERSIONED_LEN`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

impl fmt::Display for CodeAt {
    /// `path:first-last`, or the path alone without lines.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.lines {
            Some(lines) => write!(f, "{}:{lines}", self.path),
            None => f.write_str(&self.path),
        }
    }
}

/// A place a post points at: where it was made, which never changes, and
/// where Shoal followed it to since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRef {
    pub id: i64,
    /// The space whose worktree it is in, still named once the space is
    /// gone.
    pub space: ThreadSpace,
    /// The worktree's HEAD when it was made, none before a first commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitId>,
    pub original: CodeAt,
    /// The original lines as text, shown once the code is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Where Shoal last followed it to.
    pub latest: CodeAt,
    pub state: CodeState,
}

impl CodeRef {
    /// Where it is now, as `path:first-last`, with its state when its code
    /// changed.
    pub fn place(&self) -> String {
        match self.state {
            CodeState::Unchanged => self.latest.to_string(),
            state => format!("{} ({state})", self.latest),
        }
    }

    /// Its snippet, each line on a line of its own after `indent`.
    pub fn snippet_lines(&self, indent: &str) -> String {
        let lines = self.snippet.iter().flat_map(|snippet| snippet.lines());
        (lines.map(|line| format!("\n{}", format!("{indent}{line}").trim_end()))).collect()
    }
}

/// The lines `lines` of `text`, as long as [`MAX_SNIPPET_LEN`] allows,
/// whole lines only, marked where cut.
pub fn snippet(text: &str, lines: Lines) -> String {
    let start = lines.start as usize;
    let count = (lines.end - lines.start) as usize;
    let mut snippet = String::new();
    for line in text.split_inclusive('\n').skip(start).take(count) {
        if snippet.len() + line.len() > MAX_SNIPPET_LEN {
            snippet.push_str("…\n");
            break;
        }
        snippet.push_str(line);
    }
    snippet.trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests;
