//! Snapshots of a worktree's files and what changed between two. A snapshot
//! is the tree of HEAD's files with the unsaved ones Git does not ignore.
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use anyhow::{Context as _, Result, bail};
use shoal_protocol::{
    changes::{ChangeKind, Hunk},
    code::MAX_VERSIONED_LEN,
};

use super::{run_isolated, temporary_index::TemporaryIndex};

/// A file that changed between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// Its path, relative to the worktree's root.
    pub path: String,
    /// Its version after, none once deleted.
    pub blob: Option<String>,
    pub kind: ChangeKind,
    /// Its size after, or before once deleted, in bytes.
    pub size: u64,
    /// Its hunks, for a text file.
    pub hunks: Vec<Hunk>,
}

pub async fn snapshot(worktree: &Path) -> Result<String> {
    TemporaryIndex::copy_of(worktree).await?.tree().await
}

pub async fn blob_in(worktree: &Path, tree: &str, path: &str) -> Result<Option<String>> {
    let listed = run_isolated(
        worktree,
        &["ls-tree", "-z", "--object-only", tree, "--", path],
    )
    .await?;
    let blob = listed.trim_end_matches('\0').trim();
    Ok((!blob.is_empty()).then(|| blob.to_owned()))
}

/// Whether Git ignores the file `path` of the worktree, which a snapshot
/// leaves out.
pub async fn ignored(worktree: &Path, path: &str) -> Result<bool> {
    let mut command = super::isolated_command(worktree);
    command.args(["check-ignore", "-q", "--", path]);
    Ok(super::output_unless(command, 1).await?.is_some())
}

/// Whether the repository of `worktree` still has the snapshot `tree`,
/// which no ref keeps, so a prune may have deleted it.
pub async fn exists(worktree: &Path, tree: &str) -> Result<bool> {
    let mut command = super::isolated_command(worktree);
    command.args(["cat-file", "-e", tree]);
    Ok(super::output_unless(command, 1).await?.is_some())
}

/// The tree `tree` with the file `path` at version `blob`, or without it.
pub async fn with_version(
    worktree: &Path,
    tree: &str,
    path: &str,
    blob: Option<&str>,
) -> Result<String> {
    let index = TemporaryIndex::empty(worktree)?;
    index.run(&["read-tree", tree]).await?;
    match blob {
        Some(blob) => {
            let entry = format!("100644,{blob},{path}");
            (index.run(&["update-index", "--add", "--cacheinfo", &entry])).await?
        }
        None => (index.run(&["update-index", "--force-remove", "--", path])).await?,
    };
    Ok(index.run(&["write-tree"]).await?.trim().to_owned())
}

/// The paths among `paths` that the worktree's attributes mark
/// `linguist-generated`.
pub async fn generated(worktree: &Path, paths: &[&str]) -> Result<BTreeSet<String>> {
    if paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut command = super::isolated_command(worktree);
    command.args(["check-attr", "--stdin", "-z", "linguist-generated"]);
    let input: Vec<u8> = paths
        .iter()
        .flat_map(|path| [path.as_bytes(), b"\0"].concat())
        .collect();
    let output = super::bounded(command).input(input).output().await?;
    // Each answer is its path, the attribute and its value.
    let fields: Vec<&str> = output.split('\0').collect();
    Ok((fields.as_chunks::<3>().0.iter())
        .filter(|[_, _, value]| matches!(*value, "set" | "true"))
        .map(|[path, _, _]| (*path).to_owned())
        .collect())
}

/// What changed in the worktree from the snapshot `from` to `to`: each
/// file added, changed or deleted, with its hunks while it is text.
pub async fn changed(worktree: &Path, from: &str, to: &str) -> Result<Vec<Changed>> {
    let entries = entries(worktree, from, to).await?;
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    let generated = generated(worktree, &paths).await?;
    let blobs: Vec<&str> = (entries.iter())
        .flat_map(|entry| [entry.old.as_deref(), entry.new.as_deref()])
        .flatten()
        .collect();
    let contents = Contents::read(worktree, &blobs).await?;
    Ok((entries.into_iter())
        .map(|entry| {
            let generated = generated.contains(&entry.path);
            entry.changed(&contents, generated)
        })
        .collect())
}

struct Entry {
    path: String,
    old: Option<String>,
    new: Option<String>,
}

impl Entry {
    fn changed(self, contents: &Contents, generated: bool) -> Changed {
        let old = contents.text(self.old.as_deref()).or_empty();
        let new = contents.text(self.new.as_deref()).or_empty();
        let size = contents.size(self.new.as_deref().or(self.old.as_deref()));
        let kind = match (generated, &old, &new) {
            (true, ..) => ChangeKind::Generated,
            (false, Text::Large, _) | (false, _, Text::Large) => ChangeKind::Large,
            (false, Text::Binary, _) | (false, _, Text::Binary) => ChangeKind::Binary,
            (false, ..) => ChangeKind::Text,
        };
        let hunks = match (kind, old, new) {
            (ChangeKind::Text, Text::Of(old), Text::Of(new)) => {
                shoal_protocol::changes::diff(&old, &new)
            }
            _ => Vec::new(),
        };
        Changed {
            path: self.path,
            blob: self.new,
            kind,
            size,
            hunks,
        }
    }
}

/// The regular files that differ between the trees `from` and `to`, by
/// Git's raw diff without renames, so a rename is a deletion and an
/// addition; links and submodules are left out.
async fn entries(worktree: &Path, from: &str, to: &str) -> Result<Vec<Entry>> {
    let raw = run_isolated(
        worktree,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-renames",
            from,
            to,
            "--",
        ],
    )
    .await?;
    // Each file is `:old-mode new-mode old new status`, then its path.
    let fields: Vec<&str> = raw.trim_end_matches('\0').split('\0').collect();
    let mut entries = Vec::new();
    for [header, path] in fields.as_chunks::<2>().0 {
        let parts: Vec<&str> = header.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, old, new, _status] = parts[..] else {
            bail!("unexpected raw diff entry {header}");
        };
        let regular = |mode: &str| mode == "000000" || mode.starts_with("100");
        if !(regular(old_mode) && regular(new_mode)) {
            continue;
        }
        let present = |mode: &str, blob: &str| (mode != "000000").then(|| blob.to_owned());
        entries.push(Entry {
            path: (*path).to_owned(),
            old: present(old_mode, old),
            new: present(new_mode, new),
        });
    }
    Ok(entries)
}

/// A version's contents as a change reads them.
enum Text {
    /// None: the file did not exist.
    Absent,
    Of(String),
    Binary,
    Large,
}

/// The sizes of blobs, and the contents of those small enough to diff.
struct Contents {
    sizes: HashMap<String, u64>,
    small: HashMap<String, Vec<u8>>,
}

impl Contents {
    /// The blobs `blobs` of the repository of `worktree`, read in two Git
    /// calls: their sizes, then those no larger than a change diffs.
    async fn read(worktree: &Path, blobs: &[&str]) -> Result<Self> {
        let sizes = sizes(worktree, blobs).await?;
        let small: Vec<&str> = (blobs.iter().copied())
            .filter(|blob| {
                sizes
                    .get(*blob)
                    .is_some_and(|size| *size <= MAX_VERSIONED_LEN as u64)
            })
            .collect();
        let small = read_blobs(worktree, &small).await?;
        Ok(Self { sizes, small })
    }

    fn size(&self, blob: Option<&str>) -> u64 {
        blob.and_then(|blob| self.sizes.get(blob))
            .copied()
            .unwrap_or(0)
    }

    fn text(&self, blob: Option<&str>) -> Text {
        let Some(blob) = blob else {
            return Text::Absent;
        };
        let Some(bytes) = self.small.get(blob) else {
            return Text::Large;
        };
        match std::str::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => Text::Of(text.to_owned()),
            _ => Text::Binary,
        }
    }
}

impl Text {
    /// Its text, empty for an absent file, so its diff adds or removes
    /// every line.
    fn or_empty(self) -> Self {
        match self {
            Self::Absent => Self::Of(String::new()),
            text => text,
        }
    }
}

async fn sizes(worktree: &Path, blobs: &[&str]) -> Result<HashMap<String, u64>> {
    if blobs.is_empty() {
        return Ok(HashMap::new());
    }
    let mut command = super::isolated_command(worktree);
    command.args(["cat-file", "--batch-check=%(objectname) %(objectsize)"]);
    let output = super::bounded(command)
        .input(blobs.join("\n") + "\n")
        .output()
        .await?;
    output
        .lines()
        .map(|line| {
            let (blob, size) = line.split_once(' ').context("unexpected batch line")?;
            Ok((
                blob.to_owned(),
                size.parse().context("unexpected blob size")?,
            ))
        })
        .collect()
}

async fn read_blobs(worktree: &Path, blobs: &[&str]) -> Result<HashMap<String, Vec<u8>>> {
    if blobs.is_empty() {
        return Ok(HashMap::new());
    }
    let mut command = super::isolated_command(worktree);
    command.args(["cat-file", "--batch=%(objectname) %(objectsize)"]);
    let output = (super::bounded(command)
        .input(blobs.join("\n") + "\n")
        .checked())
    .await?;
    let mut rest = &output.stdout[..];
    let mut read = HashMap::new();
    // Each blob is its name and size on a line, its bytes and a newline.
    while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
        let header = std::str::from_utf8(&rest[..end]).context("unexpected batch header")?;
        let (blob, size) = header.split_once(' ').context("unexpected batch header")?;
        let size: usize = size.parse().context("unexpected blob size")?;
        let body = (rest.get(end + 1..end + 1 + size)).context("a blob ended early")?;
        read.insert(blob.to_owned(), body.to_vec());
        rest = rest.get(end + 2 + size..).unwrap_or_default();
    }
    Ok(read)
}

#[cfg(test)]
mod tests;
