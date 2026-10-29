//! A PR's diff as the lines its hunks show, so a review's line comments
//! are checked against it before anything is posted.
use anyhow::{Context, Result, bail, ensure};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    items::{item::Current, link::ItemKind},
    review::submit::{LineComment, Side},
};

/// The files a PR's diff changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub(crate) files: Vec<FileDiff>,
    /// The commits besides the head a GitLab diff compares, which
    /// positions on it name.
    pub(crate) bases: Option<Bases>,
}

/// The merge base a GitLab diff starts from and the commit of the target
/// branch it was computed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bases {
    pub base: String,
    pub start: String,
}

/// A changed file: its path in the PR's version, or the base's for a
/// deleted file, its path in the base's version, or the PR's for an added
/// file, and the lines its hunks show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub(crate) path: String,
    pub(crate) old_path: String,
    pub(crate) lines: Vec<HunkLine>,
}

/// A line a hunk shows, by its number in the base's version and the PR's:
/// a removed line has no new number, an added line no old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HunkLine {
    pub(crate) old: Option<u64>,
    pub(crate) new: Option<u64>,
}

/// A line comment on the line of a file the diff shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed<'a> {
    pub(crate) comment: &'a LineComment,
    pub(crate) file: &'a FileDiff,
    pub(crate) line: HunkLine,
}

impl FileDiff {
    /// A file whose hunks are `patch`, a unified diff without file headers,
    /// as GitHub and GitLab report each file's.
    pub(crate) fn new(path: String, old_path: String, patch: &str) -> Result<Self> {
        let lines = hunk_lines(patch).with_context(|| format!("the diff of {path}"))?;
        Ok(Self {
            path,
            old_path,
            lines,
        })
    }

    /// Whether `comment` names this file on its side.
    fn named_by(&self, comment: &LineComment) -> bool {
        comment.path == self.path || (comment.side == Side::Old && comment.path == self.old_path)
    }

    /// The line `comment` is on, when a hunk shows it.
    fn line(&self, comment: &LineComment) -> Option<HunkLine> {
        let number = Some(comment.line);
        self.lines.iter().copied().find(|line| match comment.side {
            Side::New => line.new == number,
            Side::Old => line.old == number,
        })
    }
}

impl ForgeRepo {
    /// The diff of the PR `current`, read at its head commit: refused when
    /// the PR gained commits while the forge was asked.
    pub async fn pr_diff(&self, account: &Account, current: &Current) -> Result<Diff> {
        let number = current.item.number;
        let diff = self.kind.pr_diff(self, account, current).await?;
        let now = self.read(account, ItemKind::Pr, number).await?;
        ensure!(
            now.head_sha == current.head_sha,
            "PR #{number} gained commits while Shoal read its diff; try again"
        );
        Ok(diff)
    }
}

impl Diff {
    pub(crate) fn new(files: Vec<FileDiff>) -> Self {
        Self { files, bases: None }
    }

    /// The files of a Git diff, as Forgejo and Bitbucket serve a PR's.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut files = Vec::new();
        let mut section: Option<Vec<&str>> = None;
        for line in text.lines() {
            if line.starts_with("diff --git ") {
                files.extend(
                    section
                        .take()
                        .map(|lines| file_section(&lines))
                        .transpose()?,
                );
            }
            match &mut section {
                Some(lines) => lines.push(line),
                None if line.starts_with("diff --git ") => section = Some(vec![line]),
                None => ensure!(
                    line.trim().is_empty(),
                    "the diff does not start with a file"
                ),
            }
        }
        files.extend(section.map(|lines| file_section(&lines)).transpose()?);
        Ok(Self::new(files))
    }

    /// Each comment on the line the diff shows for it, refusing all of
    /// them, naming each off the diff, when any is.
    pub fn place<'a>(&'a self, comments: &'a [LineComment]) -> Result<Vec<Placed<'a>>> {
        let mut placed = Vec::new();
        let mut off = Vec::new();
        for comment in comments {
            match self.files.iter().find(|file| file.named_by(comment)) {
                None => off.push(format!(
                    "{comment}: the diff does not change {}",
                    comment.path
                )),
                Some(file) => match file.line(comment) {
                    Some(line) => placed.push(Placed {
                        comment,
                        file,
                        line,
                    }),
                    None => off.push(format!("{comment}: no hunk of the diff shows this line")),
                },
            }
        }
        if !off.is_empty() {
            bail!(
                "these line comments are not on lines the PR's diff shows, so nothing was sent:\n  {}",
                off.join("\n  ")
            );
        }
        Ok(placed)
    }
}

/// The file a `diff --git` section of a Git diff describes.
fn file_section(lines: &[&str]) -> Result<FileDiff> {
    let hunks = lines
        .iter()
        .position(|line| line.starts_with("@@"))
        .unwrap_or(lines.len());
    let (mut old, mut new) = (None, None);
    for line in &lines[..hunks] {
        if let Some(path) = line.strip_prefix("--- ") {
            old = side_path(path, "a/");
        } else if let Some(path) = line.strip_prefix("+++ ") {
            new = side_path(path, "b/");
        } else if let Some(path) = line.strip_prefix("rename from ") {
            old = Some(unquote(path));
        } else if let Some(path) = line.strip_prefix("rename to ") {
            new = Some(unquote(path));
        }
    }
    // A file without hunks, such as a binary or a mode change, names its
    // paths only in the section's first line.
    if old.is_none() && new.is_none() {
        let paths = lines[0].trim_start_matches("diff --git ");
        let (a, b) = git_paths(paths).with_context(|| format!("unreadable diff line {paths}"))?;
        (old, new) = (Some(a), Some(b));
    }
    let path = new.clone().or_else(|| old.clone()).unwrap_or_default();
    let old_path = old.unwrap_or_else(|| path.clone());
    FileDiff::new(path, old_path, &lines[hunks..].join("\n"))
}

/// The path a `---` or `+++` line names without its `prefix`; none for
/// `/dev/null`, the side of an added or deleted file.
fn side_path(path: &str, prefix: &str) -> Option<String> {
    let path = unquote(path.trim_end_matches('\t'));
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(&path).to_owned())
}

/// The two paths of a `diff --git a/X b/Y` line, which are the same path
/// unless the section also names them on rename lines.
fn git_paths(paths: &str) -> Option<(String, String)> {
    if paths.starts_with('"') {
        let (a, b) = paths.split_once("\" ")?;
        let a = unquote(&format!("{a}\""));
        let b = unquote(b);
        return Some((a.strip_prefix("a/")?.into(), b.strip_prefix("b/")?.into()));
    }
    let a = paths.strip_prefix("a/")?;
    let half = a.len().checked_sub(3)? / 2;
    let (a, b) = (a.get(..half)?, a.get(half..)?);
    let b = b.strip_prefix(" b/")?;
    (a == b).then(|| (a.to_owned(), b.to_owned()))
}

/// A path as Git writes it, in quotes with C escapes when it has unusual
/// characters.
fn unquote(path: &str) -> String {
    let Some(quoted) = path
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
    else {
        return path.to_owned();
    };
    let mut bytes = Vec::new();
    let mut chars = quoted.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next @ b'0'..=b'7') = chars.peek().copied() {
                        value = value * 8 + u32::from(next - b'0');
                        chars.next();
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The lines the hunks of `patch` show. Each hunk's header counts its
/// lines, so a removed line that reads like a file header stays in it.
fn hunk_lines(patch: &str) -> Result<Vec<HunkLine>> {
    let mut lines = Vec::new();
    let mut remaining = (0, 0);
    let (mut old, mut new) = (0, 0);
    for text in patch.lines() {
        if remaining == (0, 0) {
            if text.starts_with("@@") {
                (old, new, remaining) = hunk_header(text)?;
            }
            continue;
        }
        let line = match text.bytes().next() {
            Some(b'+') => HunkLine {
                old: None,
                new: Some(new),
            },
            Some(b'-') => HunkLine {
                old: Some(old),
                new: None,
            },
            // Git marks a missing final newline after the line it follows.
            Some(b'\\') => continue,
            // An empty line is an unchanged empty line some forges trim.
            Some(b' ') | None => HunkLine {
                old: Some(old),
                new: Some(new),
            },
            Some(_) => bail!("unreadable hunk line {text:?}"),
        };
        if line.old.is_some() {
            ensure!(remaining.0 > 0, "a hunk has more old lines than it counts");
            remaining.0 -= 1;
            old += 1;
        }
        if line.new.is_some() {
            ensure!(remaining.1 > 0, "a hunk has more new lines than it counts");
            remaining.1 -= 1;
            new += 1;
        }
        lines.push(line);
    }
    Ok(lines)
}

/// The first old and new line of the hunk `header` starts, and how many of
/// each it shows: `@@ -OLD[,COUNT] +NEW[,COUNT] @@`.
fn hunk_header(header: &str) -> Result<(u64, u64, (u64, u64))> {
    let ranges = header
        .strip_prefix("@@ -")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(ranges, _)| ranges)
        .with_context(|| format!("unreadable hunk header {header:?}"))?;
    let (old, new) = ranges
        .split_once(" +")
        .with_context(|| format!("unreadable hunk header {header:?}"))?;
    let range = |range: &str| -> Result<(u64, u64)> {
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        Ok((start.parse()?, count.parse()?))
    };
    let ((old, old_count), (new, new_count)) = (range(old)?, range(new)?);
    Ok((old, new, (old_count, new_count)))
}

#[cfg(test)]
mod tests;
