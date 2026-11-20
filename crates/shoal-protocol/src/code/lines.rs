//! Line ranges in a file, and where a range of one version of a file is in
//! another: a pure function over the two texts, so the daemon and an
//! editor's language server, on an unsaved buffer, follow code alike.
use std::{fmt, str::FromStr};

use anyhow::{Context as _, Result, ensure};
use imara_diff::{Algorithm, Diff, Hunk, InternedInput};
use serde::{Deserialize, Serialize};

use super::CodeState;

/// Lines `start..end` of a file, counted from 0, as editors count them; an
/// empty range is the point before line `start`. Text shows and takes them
/// counted from 1, as `FIRST-LAST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lines {
    pub start: u32,
    pub end: u32,
}

impl Lines {
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

impl fmt::Display for Lines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let first = self.start + 1;
        match self.end {
            end if end <= first => write!(f, "{first}"),
            last => write!(f, "{first}-{last}"),
        }
    }
}

impl FromStr for Lines {
    type Err = anyhow::Error;

    /// `FIRST-LAST`, or `LINE` alone, counted from 1.
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || format!("invalid lines {text}: expected FIRST-LAST or LINE, from 1");
        let (first, last) = text.split_once('-').unwrap_or((text, text));
        let first: u32 = first.parse().ok().with_context(invalid)?;
        let last: u32 = last.parse().ok().with_context(invalid)?;
        ensure!(first >= 1 && first <= last, invalid());
        Ok(Self {
            start: first - 1,
            end: last,
        })
    }
}

/// Where a range is in a later version of its file, and what became of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Followed {
    pub lines: Lines,
    pub state: CodeState,
}

/// Where `lines` of `before` are in `after`: shifted by the lines added or
/// removed above them while nothing inside changed, grown or shrunk to
/// cover what replaced lines inside them, or the point where they were
/// once all of them are deleted. A point stays a point and `outdated`.
pub fn follow(before: &str, after: &str, lines: Lines) -> Followed {
    let input = InternedInput::new(before, after);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let hunks: Vec<Hunk> = diff.hunks().collect();
    follow_hunks(&hunks, lines)
}

/// [`follow`] over the hunks of the diff, in order.
fn follow_hunks(hunks: &[Hunk], lines: Lines) -> Followed {
    let start = start_after(hunks, lines.start);
    if lines.is_empty() {
        return Followed {
            lines: Lines { start, end: start },
            state: CodeState::Outdated,
        };
    }
    let end = end_after(hunks, lines.end).max(start);
    let touched =
        (hunks.iter()).any(|hunk| hunk.before.start < lines.end && hunk.before.end > lines.start);
    let state = match (start == end, touched) {
        (true, _) => CodeState::Outdated,
        (false, true) => CodeState::Changed,
        (false, false) => CodeState::Unchanged,
    };
    Followed {
        lines: Lines { start, end },
        state,
    }
}

/// Where the range starting at line `line` starts after `hunks`: lines
/// inserted at it go above it, and a hunk around it moves it to where its
/// replacement starts.
fn start_after(hunks: &[Hunk], line: u32) -> u32 {
    let mut shift = 0i64;
    for hunk in hunks {
        if hunk.before.end <= line {
            shift += size(hunk);
        } else if hunk.before.start < line {
            return hunk.after.start;
        } else {
            break;
        }
    }
    moved(line, shift)
}

/// Where the range ending before line `line` ends after `hunks`: lines
/// inserted at it go below it, and a hunk around it moves it to where its
/// replacement ends.
fn end_after(hunks: &[Hunk], line: u32) -> u32 {
    let mut shift = 0i64;
    for hunk in hunks.iter().take_while(|hunk| hunk.before.start < line) {
        if hunk.before.end <= line {
            shift += size(hunk);
        } else {
            return hunk.after.end;
        }
    }
    moved(line, shift)
}

/// How many lines `hunk` adds, less those it removes.
fn size(hunk: &Hunk) -> i64 {
    i64::from(hunk.after.end - hunk.after.start) - i64::from(hunk.before.end - hunk.before.start)
}

fn moved(line: u32, shift: i64) -> u32 {
    u32::try_from(i64::from(line) + shift).unwrap_or(0)
}

#[cfg(test)]
mod tests;
