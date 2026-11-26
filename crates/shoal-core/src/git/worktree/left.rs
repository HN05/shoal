//! The files left of a worktree Git disowned, read against a commit of its
//! repository. Git no longer has the worktree's HEAD, index or ignore rules,
//! so a file left counts as unsaved where it differs from the commit, and a
//! file the commit has that is gone counts as the removal's deletion.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use shoal_refs::CommitId;
use tokio::process::Command;

use crate::fsutil::off_runtime;

/// The files left in a directory Git disowned, laid over `head`: a
/// temporary index holds the commit's files, those missing from the
/// directory marked skip-worktree, so Git takes them as unchanged and reads
/// the commit's ignore rules where their files are gone.
pub struct LeftFiles {
    git_dir: PathBuf,
    dir: PathBuf,
    head: CommitId,
    _index_dir: tempfile::TempDir,
    index: PathBuf,
}

/// How Git reads submodules here: as changed when they have a new commit,
/// tracked changes or untracked files.
const SUBMODULES: &str = "--ignore-submodules=none";

const GITLINK: &str = "160000";

struct Changed {
    path: String,
    /// The commit records a submodule there.
    submodule: bool,
}

impl LeftFiles {
    /// Read the files left in `dir`, a worktree of `repo` that Git disowned,
    /// against `head`.
    pub(crate) async fn read(repo: &Path, dir: &Path, head: &CommitId) -> Result<Self> {
        let index_dir = tempfile::tempdir().context("create a temporary index directory")?;
        let left = Self {
            git_dir: super::super::common_dir(repo).await?,
            dir: dir.to_owned(),
            head: head.clone(),
            index: index_dir.path().join("index"),
            _index_dir: index_dir,
        };
        left.run(&["read-tree", head.as_str()]).await?;
        left.mark_gone().await?;
        // Without the stat data a checkout records, every file would read
        // as changed.
        left.run(&["update-index", "-q", "--refresh"]).await?;
        Ok(left)
    }

    async fn mark_gone(&self) -> Result<()> {
        let gone = self.run(&["ls-files", "-z", "--deleted"]).await?;
        let command = self.command(&["update-index", "-z", "--skip-worktree", "--stdin"]);
        super::super::bounded(command)
            .input(gone)
            .output()
            .await
            .context("mark the files gone from the directory")
            .map(drop)
    }

    pub fn head(&self) -> &CommitId {
        &self.head
    }

    /// The files that differ from the commit's, as `git status --porcelain`
    /// lists them, paths quoted as Git quotes them: a changed one with its
    /// status letter, an untracked one with `??`.
    pub(crate) async fn status(&self) -> Result<String> {
        let changed = self
            .run(&["diff-files", "--name-status", SUBMODULES])
            .await?;
        let untracked = (self.run(&["ls-files", "--others", "--exclude-standard"])).await?;
        let changed = (changed.lines())
            .filter_map(|line| line.split_once('\t'))
            .map(|(status, path)| format!(" {status} {path}\n"));
        let untracked = untracked.lines().map(|path| format!("?? {path}\n"));
        Ok(changed.chain(untracked).collect())
    }

    /// The size of the files a snapshot would add to the commit, as
    /// [`crate::git::checkpoint::unsaved_bytes`] counts it for a worktree:
    /// `None` when a directory left is a nested repository or a submodule
    /// changed, since a snapshot records only the commit of either.
    pub async fn unsaved_bytes(&self) -> Result<Option<u64>> {
        let changed = self.changed().await?;
        let untracked = self.untracked().await?;
        // Git lists a nested repository as its directory, without descending.
        if untracked.iter().any(|file| file.ends_with('/'))
            || changed.iter().any(|file| file.submodule)
        {
            return Ok(None);
        }
        let paths: Vec<PathBuf> = (changed.into_iter().map(|file| file.path))
            .chain(untracked)
            .map(|path| self.dir.join(path))
            .collect();
        off_runtime::run(move || {
            // A file deleted meanwhile adds nothing.
            Ok(Some(
                (paths.iter())
                    .filter_map(|path| std::fs::symlink_metadata(path).ok())
                    .map(|metadata| metadata.len())
                    .sum(),
            ))
        })
        .await
    }

    /// A commit on the commit read against with the files left laid over
    /// its own, ignored ones excepted, or that commit itself when nothing
    /// left differs from it.
    pub async fn snapshot(&self, message: &str) -> Result<CommitId> {
        if self.status().await?.is_empty() {
            return Ok(self.head.clone());
        }
        self.run(&["add", "--all", "--", ":/"]).await?;
        let tree = self.run(&["write-tree"]).await?;
        let commit = self
            .run(&[
                "commit-tree",
                tree.trim(),
                "-p",
                self.head.as_str(),
                "-m",
                message,
            ])
            .await?;
        Ok(commit.trim().into())
    }

    async fn changed(&self) -> Result<Vec<Changed>> {
        let listed = self.run(&["diff-files", "-z", "--raw", SUBMODULES]).await?;
        let mut fields = listed.split_terminator('\0');
        let mut changed = Vec::new();
        // Each entry is `:<old mode> <new mode> <ids> <status>`, then its path.
        while let (Some(entry), Some(path)) = (fields.next(), fields.next()) {
            let mut modes = entry.trim_start_matches(':').split(' ').take(2);
            changed.push(Changed {
                path: path.to_owned(),
                submodule: modes.any(|mode| mode == GITLINK),
            });
        }
        Ok(changed)
    }

    /// The files the commit lacks, ignored ones excepted.
    async fn untracked(&self) -> Result<Vec<String>> {
        let listed = (self.run(&["ls-files", "-z", "--others", "--exclude-standard"])).await?;
        Ok(listed.split_terminator('\0').map(str::to_owned).collect())
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        super::super::output(self.command(args)).await
    }

    /// Git working on the directory with the repository's objects and the
    /// temporary index.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = super::super::isolated_command_without_submodules(&self.dir);
        command
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.dir)
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_AUTHOR_NAME", "Shoal")
            .env("GIT_AUTHOR_EMAIL", "shoal@localhost")
            .env("GIT_COMMITTER_NAME", "Shoal")
            .env("GIT_COMMITTER_EMAIL", "shoal@localhost")
            .args(args);
        command
    }
}

#[cfg(test)]
mod tests;
