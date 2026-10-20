//! Restore points: commits that keep a removed worktree's HEAD and unsaved
//! files reachable under a private ref until the restore record expires.
use std::path::Path;

use anyhow::{Context, Result};

use super::{isolated_command_without_submodules, run_isolated};
use crate::subprocess;

const REFS: &str = "refs/shoal/removed/";

/// The ref that keeps a removed workspace's restore point.
pub fn reference(workspace_id: &str) -> String {
    format!("{REFS}{workspace_id}")
}

/// The workspace IDs with a restore point ref in the repository.
pub async fn workspace_ids(repo: &Path) -> Result<Vec<String>> {
    let refs = run_isolated(repo, &["for-each-ref", "--format=%(refname)", REFS]).await?;
    Ok(refs
        .lines()
        .filter_map(|reference| reference.strip_prefix(REFS))
        .map(str::to_owned)
        .collect())
}

/// The size of the files a snapshot would add to HEAD: tracked changes and
/// untracked files, excluding ignored ones. `None` when an untracked directory
/// is a nested repository, whose files a snapshot cannot keep.
pub async fn unsaved_bytes(worktree: &Path) -> Result<Option<u64>> {
    let changed = run_isolated(worktree, &["diff", "-z", "--name-only", "HEAD", "--"]).await?;
    let untracked = run_isolated(
        worktree,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )
    .await?;
    // Git lists a nested repository as its directory, without descending.
    if untracked.split('\0').any(|file| file.ends_with('/')) {
        return Ok(None);
    }
    let mut bytes = 0;
    for file in [changed, untracked]
        .iter()
        .flat_map(|files| files.split('\0'))
        .filter(|file| !file.is_empty())
    {
        // A deleted tracked file is listed as changed and adds nothing.
        if let Ok(metadata) = std::fs::symlink_metadata(worktree.join(file)) {
            bytes += metadata.len();
        }
    }
    Ok(Some(bytes))
}

/// A commit of the worktree's files, ignored ones excepted, on top of HEAD;
/// HEAD itself when nothing changed. A temporary index keeps the worktree's
/// own index and files untouched.
pub async fn snapshot(worktree: &Path, message: &str) -> Result<String> {
    let head = run_isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let head = head.trim().to_owned();
    let status = run_isolated(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    if status.is_empty() {
        return Ok(head);
    }
    let index = tempfile::tempdir().context("create a temporary index directory")?;
    let index = index.path().join("index");
    let in_index = async |args: &[&str]| {
        let mut command = isolated_command_without_submodules(worktree);
        command
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "Shoal")
            .env("GIT_AUTHOR_EMAIL", "shoal@localhost")
            .env("GIT_COMMITTER_NAME", "Shoal")
            .env("GIT_COMMITTER_EMAIL", "shoal@localhost")
            .args(args);
        subprocess::output(command).await
    };
    in_index(&["read-tree", &head]).await?;
    in_index(&["add", "--all", "--", ":/"]).await?;
    let tree = in_index(&["write-tree"]).await?;
    let commit = in_index(&["commit-tree", tree.trim(), "-p", &head, "-m", message]).await?;
    Ok(commit.trim().to_owned())
}

/// Point `reference` at `commit`, keeping it reachable.
pub async fn save(repo: &Path, reference: &str, commit: &str) -> Result<()> {
    run_isolated(repo, &["update-ref", reference, commit]).await?;
    Ok(())
}

pub async fn delete(repo: &Path, reference: &str) -> Result<()> {
    run_isolated(repo, &["update-ref", "-d", reference]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::{git, repository};

    #[tokio::test]
    async fn snapshot_keeps_changes_and_untracked_files_without_touching_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
        crate::test_support::commit(&repo, ".gitignore");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(snapshot(&repo, "clean").await.unwrap(), head.trim());

        fs::write(repo.join("tracked"), "changed\n").unwrap();
        fs::write(repo.join("staged"), "staged\n").unwrap();
        git(&repo, &["add", "staged"]);
        fs::create_dir(repo.join("scratch")).unwrap();
        fs::write(repo.join("scratch/notes"), "notes\n").unwrap();
        fs::write(repo.join("ignored"), "build output\n").unwrap();
        let status = git(&repo, &["status", "--porcelain"]);
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), Some(8 + 7 + 6));

        let commit = snapshot(&repo, "restore point").await.unwrap();
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        assert_eq!(git(&repo, &["rev-parse", &format!("{commit}^")]), head);
        let files = git(&repo, &["ls-tree", "-r", "--name-only", &commit]);
        assert_eq!(files, ".gitignore\nscratch/notes\nstaged\ntracked\n");
        assert_eq!(
            git(&repo, &["show", &format!("{commit}:tracked")]),
            "changed\n"
        );

        let nested = repository(&repo, "nested");
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), None);
        fs::remove_dir_all(nested).unwrap();

        let reference = reference("workspace");
        save(&repo, &reference, &commit).await.unwrap();
        assert_eq!(workspace_ids(&repo).await.unwrap(), ["workspace"]);
        delete(&repo, &reference).await.unwrap();
        assert!(workspace_ids(&repo).await.unwrap().is_empty());
    }
}
