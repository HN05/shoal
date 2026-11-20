//! Change entries: what one turn of one agent changed for a thread, which
//! its history shows between its posts. A thread's entries stay within
//! [`MAX_ENTRIES_BYTES`] however much work it holds, so it still fits one
//! response: older entries sum up their files, then merge.
use serde::{Deserialize, Serialize};
use shoal_ids::execution_ids::ExecutionId;

use super::{Change, ChangeKind};
use crate::threads::ThreadSpace;

/// Most bytes a thread's change entries take as JSON.
pub const MAX_ENTRIES_BYTES: usize = 8 * 1024;

/// What one turn of one agent changed for a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// When its first change was made.
    pub at: crate::time::UnixTime,
    /// The space whose worktree it changed.
    pub space: ThreadSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionId>,
    /// The numbers of its changes in that space, as ranges from the first
    /// to the last.
    pub numbers: Vec<[u32; 2]>,
    /// Each file it changed, in the order it first changed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<EntryFile>,
    /// The text files it changed that it sums up without naming them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<MoreFiles>,
}

/// A file a change entry changed, with the lines its hunks added and
/// removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryFile {
    pub path: String,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
}

/// Text files an entry no longer names, with the lines their hunks added
/// and removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoreFiles {
    pub files: u32,
    pub added: u64,
    pub removed: u64,
}

/// What a change entry counts of one change, which a thread's reading
/// selects without the change's lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counted {
    pub number: u32,
    pub space: ThreadSpace,
    pub kind: ChangeKind,
    pub made_at: crate::time::UnixTime,
    pub turn: Option<i64>,
    pub execution: Option<ExecutionId>,
    /// Where its file is now.
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

impl From<&Change> for Counted {
    fn from(change: &Change) -> Self {
        Self {
            number: change.number,
            space: change.space.clone(),
            kind: change.kind,
            made_at: change.made_at,
            turn: change.turn,
            execution: change.execution.clone(),
            path: change.latest.path.clone(),
            added: change.added,
            removed: change.removed,
        }
    }
}

impl ChangeEntry {
    fn of(change: &Counted) -> Self {
        let mut entry = Self {
            at: change.made_at,
            space: change.space.clone(),
            turn: change.turn,
            execution: change.execution.clone(),
            numbers: Vec::new(),
            files: Vec::new(),
            more: None,
        };
        entry.take(change);
        entry
    }

    /// Count `change`, the next of the entry, in it.
    fn take(&mut self, change: &Counted) {
        self.number(change.number);
        let path = &change.path;
        let file = match self.files.iter().position(|file| file.path == *path) {
            Some(at) => &mut self.files[at],
            None => {
                self.files.push(EntryFile {
                    path: path.clone(),
                    kind: change.kind,
                    added: 0,
                    removed: 0,
                });
                self.files.last_mut().expect("a file was just added")
            }
        };
        file.added += change.added;
        file.removed += change.removed;
    }

    /// Add `number` to its numbers, extending the last range it follows.
    fn number(&mut self, number: u32) {
        match self.numbers.last_mut() {
            Some([_, last]) if *last + 1 == number => *last = number,
            _ => self.numbers.push([number, number]),
        }
    }

    /// Whether `change` belongs to it: made in the same space, turn and
    /// execution.
    fn holds(&self, change: &Counted) -> bool {
        self.space.id == change.space.id
            && self.turn == change.turn
            && self.execution == change.execution
    }

    /// Sum up its text files without naming them, and leave out the files
    /// without hunks, which count in no size.
    fn sum_up(&mut self) {
        let more = self.more.get_or_insert_with(MoreFiles::default);
        for file in self
            .files
            .drain(..)
            .filter(|file| file.kind == ChangeKind::Text)
        {
            more.files += 1;
            more.added += u64::from(file.added);
            more.removed += u64::from(file.removed);
        }
    }

    /// Take the later entry `later` into it, summed up.
    fn merge(&mut self, mut later: Self) {
        later.sum_up();
        self.sum_up();
        for [first, last] in later.numbers {
            match self.numbers.last_mut() {
                Some([_, end]) if *end + 1 == first => *end = last,
                _ => self.numbers.push([first, last]),
            }
        }
        let (more, taken) = (self.more.get_or_insert_with(MoreFiles::default), later.more);
        let taken = taken.unwrap_or_default();
        more.files += taken.files;
        more.added += taken.added;
        more.removed += taken.removed;
        if self.space.id != later.space.id {
            (self.turn, self.execution) = (None, None);
        }
    }

    /// Its numbers as text names them, such as `4–9, 12`.
    pub fn numbers_text(&self) -> String {
        let ranges = self
            .numbers
            .iter()
            .map(|[first, last]| match first == last {
                true => first.to_string(),
                false => format!("{first}–{last}"),
            });
        ranges.collect::<Vec<_>>().join(", ")
    }
}

/// The change entries of `changes`, in the order they were made: one per
/// run of changes of the same space, turn and execution that no post made
/// at one of `posts` falls inside, so a history shows the work after a
/// post below it; within [`MAX_ENTRIES_BYTES`].
pub fn entries(
    changes: impl IntoIterator<Item = Counted>,
    posts: &[crate::time::UnixTime],
) -> Vec<ChangeEntry> {
    let mut entries: Vec<ChangeEntry> = Vec::new();
    let mut last = None;
    for change in changes {
        let change = &change;
        let posted = |since: crate::time::UnixTime| {
            (posts.iter()).any(|post| since < *post && *post <= change.made_at)
        };
        let continues = last.is_some_and(|since| !posted(since));
        last = Some(change.made_at);
        match entries.last_mut() {
            Some(entry) if continues && entry.holds(change) => entry.take(change),
            _ => entries.push(ChangeEntry::of(change)),
        }
    }
    within_budget(entries)
}

/// `entries` within [`MAX_ENTRIES_BYTES`]: the oldest first sum up their
/// files, then merge into the oldest, which keeps only the span of its
/// numbers once nothing else fits. Each entry's size is measured once and
/// again only when it changes.
fn within_budget(mut entries: Vec<ChangeEntry>) -> Vec<ChangeEntry> {
    let mut sizes: Vec<usize> = entries.iter().map(json_len).collect();
    // A list's brackets, and a comma between each two entries.
    let total = |sizes: &[usize]| sizes.iter().sum::<usize>() + sizes.len().max(1) + 1;
    let mut next = 0;
    while total(&sizes) > MAX_ENTRIES_BYTES && next < entries.len() {
        entries[next].sum_up();
        sizes[next] = json_len(&entries[next]);
        next += 1;
    }
    while total(&sizes) > MAX_ENTRIES_BYTES && entries.len() > 1 {
        let later = entries.remove(1);
        sizes.remove(1);
        entries[0].merge(later);
        sizes[0] = json_len(&entries[0]);
    }
    if total(&sizes) > MAX_ENTRIES_BYTES
        && let Some(entry) = entries.first_mut()
        && let (Some([first, _]), Some([_, last])) = (entry.numbers.first(), entry.numbers.last())
    {
        entry.numbers = vec![[*first, *last]];
    }
    entries
}

/// How many bytes `entry` takes as JSON.
fn json_len(entry: &ChangeEntry) -> usize {
    serde_json::to_vec(entry).map_or(usize::MAX / 2, |json| json.len())
}

#[cfg(test)]
mod tests;
