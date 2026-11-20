//! A temporary index, so Shoal writes trees of a worktree's files, or of a
//! commit it changes, without touching the worktree's own index or files.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::isolated_command_without_submodules;

pub(super) struct TemporaryIndex<'a> {
    worktree: &'a Path,
    _dir: tempfile::TempDir,
    index: PathBuf,
}

impl<'a> TemporaryIndex<'a> {
    /// A copy of the worktree's index, which keeps paths staged despite
    /// ignore rules.
    pub(super) async fn copy_of(worktree: &'a Path) -> Result<Self> {
        let index = Self::empty(worktree)?;
        let own = super::git_path(worktree, "index").await?;
        std::fs::copy(own, &index.index).context("copy the worktree's index")?;
        Ok(index)
    }

    /// An index of nothing yet.
    pub(super) fn empty(worktree: &'a Path) -> Result<Self> {
        let dir = tempfile::tempdir().context("create a temporary index directory")?;
        let index = dir.path().join("index");
        Ok(Self {
            worktree,
            _dir: dir,
            index,
        })
    }

    /// The tree of every file in the worktree, ignored ones excepted unless
    /// staged.
    pub(super) async fn tree(&self) -> Result<String> {
        self.run(&["add", "--all", "--", ":/"]).await?;
        Ok(self.run(&["write-tree"]).await?.trim().to_owned())
    }

    /// Git in the worktree over this index, committing as Shoal.
    pub(super) async fn run(&self, args: &[&str]) -> Result<String> {
        let mut command = isolated_command_without_submodules(self.worktree);
        command
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_AUTHOR_NAME", "Shoal")
            .env("GIT_AUTHOR_EMAIL", "shoal@localhost")
            .env("GIT_COMMITTER_NAME", "Shoal")
            .env("GIT_COMMITTER_EMAIL", "shoal@localhost")
            .args(args);
        super::output(command).await
    }
}
