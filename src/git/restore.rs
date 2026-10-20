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
/// is a nested repository or a submodule changed, since a snapshot records
/// only the commit of either, not its files.
pub async fn unsaved_bytes(worktree: &Path) -> Result<Option<u64>> {
    let changed = run_isolated(worktree, &["diff", "-z", "--name-only", "HEAD", "--"]).await?;
    let untracked = run_isolated(
        worktree,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )
    .await?;
    // Git lists a nested repository as its directory, without descending.
    if untracked.split('\0').any(|file| file.ends_with('/')) || changed_submodule(worktree).await? {
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

/// Whether a submodule has a new commit, tracked changes or untracked files.
async fn changed_submodule(worktree: &Path) -> Result<bool> {
    let status = run_isolated(
        worktree,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=none",
        ],
    )
    .await?;
    let mut entries = status.split('\0');
    while let Some(entry) = entries.next() {
        let mut fields = entry.split(' ');
        let kind = fields.next();
        // Changed entries name their submodule state third: `S` and flags.
        if matches!(kind, Some("1" | "2")) && fields.nth(1).is_some_and(|sub| sub.starts_with('S'))
        {
            return Ok(true);
        }
        // A rename's original path follows as its own entry.
        if kind == Some("2") {
            entries.next();
        }
    }
    Ok(false)
}

/// A commit on top of HEAD with the worktree's files, ignored ones excepted
/// unless staged, or with HEAD's tree when `files` is false, keeping `earlier` reachable as
/// its second parent; HEAD itself when it would add neither. A temporary
/// index keeps the worktree's own index and files untouched.
pub async fn snapshot(
    worktree: &Path,
    message: &str,
    files: bool,
    earlier: Option<&str>,
) -> Result<String> {
    let head = run_isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let head = head.trim().to_owned();
    let earlier = earlier.filter(|earlier| *earlier != head);
    let status = run_isolated(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    let files = files && !status.is_empty();
    if !files && earlier.is_none() {
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
    if files {
        // The worktree's own index keeps paths staged despite ignore rules.
        let own = run_isolated(
            worktree,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .await?;
        std::fs::copy(own.trim(), &index).context("copy the worktree's index")?;
        in_index(&["add", "--all", "--", ":/"]).await?;
    } else {
        in_index(&["read-tree", &head]).await?;
    }
    let tree = in_index(&["write-tree"]).await?;
    let mut args = vec!["commit-tree", tree.trim(), "-p", &head];
    args.extend(earlier.iter().flat_map(|earlier| ["-p", earlier]));
    args.extend(["-m", message]);
    let commit = in_index(&args).await?;
    Ok(commit.trim().to_owned())
}

/// Write the files of `snapshot` into the worktree, leaving the index alone:
/// changed and untracked files come back as unsaved changes, and files the
/// snapshot lacks are deleted.
pub async fn apply(worktree: &Path, snapshot: &str) -> Result<()> {
    let mut command = isolated_command_without_submodules(worktree);
    command.args([
        "restore",
        &format!("--source={snapshot}"),
        "--worktree",
        "--",
        ":/",
    ]);
    subprocess::output(command).await?;
    Ok(())
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

/// Delete `reference` only while it names `commit`; `false` when it names
/// another commit by then. A missing ref counts as deleted.
pub async fn delete_at(repo: &Path, reference: &str, commit: &str) -> Result<bool> {
    let deleted = run_isolated(repo, &["update-ref", "-d", reference, commit]).await;
    if deleted.is_ok() || !super::ref_exists(repo, reference, super::isolated_command).await? {
        return Ok(true);
    }
    let current = super::verify_revision(repo, reference, run_isolated).await?;
    if current.trim() != commit {
        return Ok(false);
    }
    deleted.map(|_| true)
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
        fs::write(repo.join(".gitignore"), "ignored\nforced\n").unwrap();
        crate::test_support::commit(&repo, ".gitignore");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(
            snapshot(&repo, "clean", true, None).await.unwrap(),
            head.trim()
        );

        fs::write(repo.join("tracked"), "changed\n").unwrap();
        fs::write(repo.join("staged"), "staged\n").unwrap();
        git(&repo, &["add", "staged"]);
        fs::create_dir(repo.join("scratch")).unwrap();
        fs::write(repo.join("scratch/notes"), "notes\n").unwrap();
        fs::write(repo.join("ignored"), "build output\n").unwrap();
        fs::write(repo.join("forced"), "forced\n").unwrap();
        git(&repo, &["add", "-f", "forced"]);
        let status = git(&repo, &["status", "--porcelain"]);
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), Some(8 + 7 + 6 + 7));

        let commit = snapshot(&repo, "restore point", true, None).await.unwrap();
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        assert_eq!(git(&repo, &["rev-parse", &format!("{commit}^")]), head);
        let files = git(&repo, &["ls-tree", "-r", "--name-only", &commit]);
        assert_eq!(
            files,
            ".gitignore\nforced\nscratch/notes\nstaged\ntracked\n"
        );
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

    #[tokio::test]
    async fn changed_submodules_cannot_be_archived() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        let sub = repository(root.path(), "sub");
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "sub",
            ],
        );
        git(&repo, &["commit", "-q", "-m", "submodule"]);
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), Some(0));
        fs::write(repo.join("sub/tracked"), "uncommitted\n").unwrap();
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), None);
        git(&repo.join("sub"), &["checkout", "-q", "--", "tracked"]);
        fs::write(repo.join("sub/untracked"), "new\n").unwrap();
        assert_eq!(unsaved_bytes(&repo).await.unwrap(), None);
    }
}
