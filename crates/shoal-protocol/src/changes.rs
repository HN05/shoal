//! Agents' changes: the hunks Shoal records as the agents it runs change
//! the files of their worktrees, who made each, and the thread it was for.
use std::{collections::BTreeSet, fmt, path::PathBuf, str::FromStr};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceNumber};
use shoal_refs::CommitId;
use shoal_states::states;

use crate::{
    code::{CodeAt, CodeState},
    item_numbers::ItemNumber,
    threads::ThreadSpace,
};

mod hunks;

pub use hunks::{Hunk, diff, header_start, split, unapply};

states!(
    /// Who made a change.
    ChangeAuthor {
        /// The space's agent, through one of its tools.
        Agent => "agent",
        /// A command of the agent's that someone else changed the same file
        /// during, so either may have made it.
        Mixed => "mixed",
        /// Someone else: the user, a formatter or a server, between the
        /// agent's tool calls.
        Others => "others",
    }
);

states!(
    /// What a change's file is, which decides whether it has hunks.
    ChangeKind {
        /// A text file, whose change is a hunk.
        Text => "text",
        /// A file the repository marks `linguist-generated`.
        Generated => "generated",
        Binary => "binary",
        /// A file over [`crate::code::MAX_VERSIONED_LEN`].
        Large => "large",
    }
);

/// What an agent's hook reports about a tool call that changes files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolChange {
    /// A tool that may change any file, such as a shell command, starts:
    /// Shoal snapshots the worktree before it.
    Starts { call: String },
    /// That tool ended: what changed since the snapshot is its change.
    Ends { call: String },
    /// An edit tool changed the file at `path`, by absolute path, as
    /// `hunks` say.
    Edited { path: PathBuf, hunks: Vec<Hunk> },
}

/// A change Shoal recorded: a hunk of one file in a space's worktree,
/// numbered in that space's own sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub number: u32,
    /// The space whose worktree it is in.
    pub space: ThreadSpace,
    pub author: ChangeAuthor,
    pub kind: ChangeKind,
    pub made_at: crate::time::UnixTime,
    /// The turn of the space's agent that made it, as the space counts
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<i64>,
    /// The agent's execution, while Shoal tracks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    /// The thread it is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ItemNumber>,
    /// The worktree's HEAD when it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitId>,
    /// Where its lines were in the version it produced, which Shoal keeps;
    /// a file without hunks has no lines.
    pub original: CodeAt,
    /// Where Shoal last followed them to.
    pub latest: CodeAt,
    pub state: CodeState,
    pub added: u32,
    pub removed: u32,
    /// The size of a file without hunks, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl Change {
    /// Whether it counts in a thread's size and editors mark its lines: a
    /// change of a text file.
    pub fn has_hunk(&self) -> bool {
        self.kind == ChangeKind::Text
    }

    /// Where it is now, with its state when its code changed since, and
    /// its kind for a file without hunks.
    pub fn place(&self) -> String {
        match (self.kind, self.state) {
            (ChangeKind::Text, CodeState::Unchanged) => self.latest.to_string(),
            (ChangeKind::Text, state) => format!("{} ({state})", self.latest),
            (kind, _) => format!("{} ({kind})", self.latest.path),
        }
    }

    /// Its number in the space it is in, as `S.N`, or `N` when that is
    /// `own`.
    pub fn label(&self, own: Option<SpaceNumber>) -> String {
        match self.space.number {
            Some(space) if Some(space) != own => format!("{space}.{}", self.number),
            _ => self.number.to_string(),
        }
    }
}

/// A change with the lines it removed and added, which a diff shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDiff {
    #[serde(flatten)]
    pub change: Change,
    /// Where it started in the version before, counted from 0.
    pub before: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_lines: Vec<String>,
}

impl ChangeDiff {
    /// Its hunk where Shoal last followed it to, as a unified diff without
    /// context shows it; none for a file without hunks.
    pub fn patch(&self) -> Option<String> {
        let lines = self.change.latest.lines?;
        let hunk = Hunk {
            before: self.before,
            lines,
            removed: self.removed_lines.clone(),
            added: self.added_lines.clone(),
        };
        Some(hunk.patch())
    }
}

/// What one turn of one agent changed for a thread, which its history
/// shows between its posts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// When its first change was made.
    pub at: crate::time::UnixTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    pub changes: Vec<Change>,
}

/// The size of a set of changes: the files whose hunks they hold, and the
/// lines added and removed; files without hunks count in none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeSize {
    pub files: usize,
    pub added: u64,
    pub removed: u64,
}

impl ChangeSize {
    pub fn of<'a>(changes: impl IntoIterator<Item = &'a Change>) -> Self {
        let mut files = BTreeSet::new();
        let mut size = Self::default();
        for change in changes.into_iter().filter(|change| change.has_hunk()) {
            files.insert((&change.space.id, &change.latest.path));
            size.added += u64::from(change.added);
            size.removed += u64::from(change.removed);
        }
        size.files = files.len();
        size
    }

    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

impl fmt::Display for ChangeSize {
    /// `3 files changed, +40 −12`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let files = match self.files {
            1 => "1 file".to_owned(),
            files => format!("{files} files"),
        };
        write!(f, "{files} changed, +{} −{}", self.added, self.removed)
    }
}

/// A change as a command names it: `S.N`, change `N` of the space numbered
/// `S`, or a bare `N` of the caller's own space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChangeNumber {
    Full(ItemNumber),
    Own(u32),
}

impl ChangeNumber {
    /// The space it names, by number, else `own`, and its number there.
    pub fn resolve(self, own: Option<SpaceNumber>) -> Result<(SpaceNumber, u32)> {
        match (self, own) {
            (Self::Full(number), _) => Ok((number.space, number.item)),
            (Self::Own(number), Some(space)) => Ok((space, number)),
            (Self::Own(number), None) => bail!(
                "change {number} names no space outside one: name it as SPACE.NUMBER, such as 3.{number}"
            ),
        }
    }
}

impl fmt::Display for ChangeNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(number) => number.fmt(f),
            Self::Own(number) => number.fmt(f),
        }
    }
}

impl FromStr for ChangeNumber {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text.contains('.') {
            return Ok(Self::Full(text.parse()?));
        }
        let number = (text.parse::<u32>().ok())
            .filter(|number| *number > 0)
            .with_context(|| {
                format!("invalid change {text}: expected NUMBER, or SPACE.NUMBER, such as 3.12")
            })?;
        Ok(Self::Own(number))
    }
}

#[cfg(test)]
mod tests;
