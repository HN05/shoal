//! Hunks: the runs of lines a change removed and added, as a unified
//! diff's hunk holds them between its unchanged lines, so an agent's edit
//! tool and Git's diff of two snapshots give the same records.
use serde::{Deserialize, Serialize};

use crate::code::Lines;

/// One run of removed and added lines, with no unchanged line inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// Where it starts in the version before, counted from 0.
    pub before: u32,
    /// Where its added lines are in the version it produced; an empty
    /// range, the point where lines were removed.
    pub lines: Lines,
    /// The lines it removed, without their newlines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// The lines it added, without their newlines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
}

impl Hunk {
    /// The hunk that adds every line of `text`, a new file's.
    pub fn created(text: &str) -> Self {
        let added: Vec<String> = text.lines().map(str::to_owned).collect();
        Self {
            before: 0,
            lines: Lines {
                start: 0,
                end: count(added.len()),
            },
            removed: Vec::new(),
            added,
        }
    }

    /// Its header and lines, as a unified diff without context shows it.
    pub fn patch(&self) -> String {
        let (removed, added) = (self.removed.len(), self.added.len());
        // A header counts from 1, and names the line before an empty side.
        let side = |start: u32, count: usize| match count {
            0 => format!("{start},0"),
            1 => (start + 1).to_string(),
            count => format!("{},{count}", start + 1),
        };
        let mut patch = format!(
            "@@ -{} +{} @@\n",
            side(self.before, removed),
            side(self.lines.start, added)
        );
        for line in &self.removed {
            patch.push_str(&format!("-{line}\n"));
        }
        for line in &self.added {
            patch.push_str(&format!("+{line}\n"));
        }
        patch
    }
}

/// The hunks of a unified diff's hunk whose `lines`, each marked `-`, `+`
/// or ` `, start at line `before` of the version before and `after` of the
/// one after, counted from 0: one per run of changed lines between its
/// unchanged ones.
pub fn split<'a>(before: u32, after: u32, lines: impl IntoIterator<Item = &'a str>) -> Vec<Hunk> {
    let (mut old, mut new) = (before, after);
    let mut hunks = Vec::new();
    let mut run: Option<Hunk> = None;
    for line in lines {
        let (mark, text) = line.split_at(line.len().min(1));
        match mark {
            "-" | "+" => {
                let hunk = run.get_or_insert_with(|| Hunk {
                    before: old,
                    lines: Lines {
                        start: new,
                        end: new,
                    },
                    removed: Vec::new(),
                    added: Vec::new(),
                });
                if mark == "-" {
                    hunk.removed.push(text.to_owned());
                    old += 1;
                } else {
                    hunk.added.push(text.to_owned());
                    new += 1;
                    hunk.lines.end = new;
                }
            }
            // A note such as "\ No newline at end of file" counts no line.
            "\\" => {}
            _ => {
                hunks.extend(run.take());
                old += 1;
                new += 1;
            }
        }
    }
    hunks.extend(run);
    hunks
}

/// The hunks that make `after` of `before`, as the code that follows
/// references diffs them.
pub fn diff(before: &str, after: &str) -> Vec<Hunk> {
    let input = imara_diff::InternedInput::new(before, after);
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let (old, new): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let text = |lines: &[&str], range: std::ops::Range<u32>| -> Vec<String> {
        let range = range.start as usize..(range.end as usize).min(lines.len());
        (lines.get(range).unwrap_or_default().iter())
            .map(|line| (*line).to_owned())
            .collect()
    };
    (diff.hunks())
        .map(|hunk| Hunk {
            before: hunk.before.start,
            lines: Lines {
                start: hunk.after.start,
                end: hunk.after.end,
            },
            removed: text(&old, hunk.before.clone()),
            added: text(&new, hunk.after.clone()),
        })
        .collect()
}

/// The text `after` was before `hunks`, each of it, made it: each hunk's
/// added lines put back to the lines it removed; none once `after` no
/// longer holds a hunk's lines where it put them.
pub fn unapply(after: &str, hunks: &[Hunk]) -> Option<String> {
    let eol = if after.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = after.split_inclusive('\n').map(str::to_owned).collect();
    let mut hunks: Vec<&Hunk> = hunks.iter().collect();
    hunks.sort_by_key(|hunk| std::cmp::Reverse(hunk.lines.start));
    for hunk in hunks {
        let range = hunk.lines.start as usize..hunk.lines.end as usize;
        let held = lines.get(range.clone())?;
        let added = hunk.added.iter().map(|line| line.trim_end_matches('\r'));
        if !held.iter().map(|line| content(line)).eq(added) {
            return None;
        }
        // The last line of a file without a final newline keeps none.
        let unterminated =
            range.end == lines.len() && lines.last().is_some_and(|line| !line.ends_with('\n'));
        let mut removed: Vec<String> = (hunk.removed.iter())
            .map(|line| format!("{}{eol}", line.trim_end_matches('\r')))
            .collect();
        if unterminated && let Some(last) = removed.last_mut() {
            last.truncate(last.len() - eol.len());
        }
        lines.splice(range, removed);
    }
    Some(lines.concat())
}

/// `line` without its line terminator.
fn content(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Where a unified diff header's side `start,count` starts, counted from 0:
/// an empty side names the line before it.
pub fn header_start(start: u32, count: u32) -> u32 {
    if count == 0 {
        start
    } else {
        start.saturating_sub(1)
    }
}

fn count(lines: usize) -> u32 {
    u32::try_from(lines).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
