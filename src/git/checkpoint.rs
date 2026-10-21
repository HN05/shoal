//! Checkpoints: commits that keep a worktree's HEAD and unsaved files
//! reachable under private refs until their records expire.
use std::path::Path;

use anyhow::{Context, Result};

use super::{isolated_command_without_submodules, run_isolated, run_without_submodules};
use crate::subprocess;

const REFS: &str = "refs/shoal/checkpoints/";
/// Where removals kept their snapshots before checkpoints replaced them.
const LEGACY_REFS: &str = "refs/shoal/removed/";

/// The ref that keeps a workspace's checkpoint.
pub fn reference(workspace_id: &str, number: u64) -> String {
    format!("{REFS}{workspace_id}/{number}")
}

/// A checkpoint ref in the repository and the commit it keeps.
pub struct SavedRef {
    pub reference: String,
    /// The workspace ID and checkpoint number the ref names; `None` for a
    /// name no checkpoint has.
    pub checkpoint: Option<(String, u64)>,
    pub commit: String,
}

/// Every checkpoint ref in the repository.
pub async fn saved_refs(repo: &Path) -> Result<Vec<SavedRef>> {
    Ok(refs(repo, REFS)
        .await?
        .into_iter()
        .map(|(reference, commit)| SavedRef {
            checkpoint: reference.strip_prefix(REFS).and_then(|name| {
                let (workspace_id, number) = name.rsplit_once('/')?;
                Some((workspace_id.to_owned(), number.parse().ok()?))
            }),
            reference,
            commit,
        })
        .collect())
}

/// The refs removals kept before checkpoints, by workspace ID, with the
/// commit each keeps.
pub async fn legacy_refs(repo: &Path) -> Result<Vec<(String, String, String)>> {
    Ok(refs(repo, LEGACY_REFS)
        .await?
        .into_iter()
        .filter_map(|(reference, commit)| {
            let workspace_id = reference.strip_prefix(LEGACY_REFS)?.to_owned();
            Some((reference, workspace_id, commit))
        })
        .collect())
}

async fn refs(repo: &Path, prefix: &str) -> Result<Vec<(String, String)>> {
    let refs = run_isolated(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )
    .await?;
    Ok(refs
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(reference, commit)| (reference.to_owned(), commit.to_owned()))
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
/// unless staged, or HEAD itself when `files` is false or nothing is unsaved.
/// A temporary index keeps the worktree's own index and files untouched.
pub async fn snapshot(worktree: &Path, message: &str, files: bool) -> Result<String> {
    let head = run_isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let head = head.trim().to_owned();
    let status = run_isolated(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    if !files || status.is_empty() {
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
    // The worktree's own index keeps paths staged despite ignore rules.
    let own = run_isolated(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await?;
    std::fs::copy(own.trim(), &index).context("copy the worktree's index")?;
    in_index(&["add", "--all", "--", ":/"]).await?;
    let tree = in_index(&["write-tree"]).await?;
    let commit = in_index(&["commit-tree", tree.trim(), "-p", &head, "-m", message]).await?;
    Ok(commit.trim().to_owned())
}

/// Whether two snapshots keep the same files.
pub async fn same_files(repo: &Path, first: &str, second: &str) -> Result<bool> {
    let trees = run_isolated(
        repo,
        &[
            "rev-parse",
            &format!("{first}^{{tree}}"),
            &format!("{second}^{{tree}}"),
        ],
    )
    .await?;
    let mut trees = trees.lines();
    Ok(trees.next() == trees.next())
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

/// Move the worktree's branch, or its detached HEAD, to `head` and replace
/// its unsaved files with those of `snapshot`, deleting tracked changes and
/// untracked files the snapshot lacks. Untracked files are cleaned under the
/// current ignore rules, before the reset can change them, so ignored files
/// stay; [`ignored_paths_in`] finds those the snapshot would overwrite.
pub async fn rewind(worktree: &Path, head: &str, snapshot: &str) -> Result<()> {
    run_without_submodules(worktree, &["clean", "--quiet", "--force", "-d", "--", ":/"]).await?;
    run_without_submodules(worktree, &["reset", "--quiet", "--hard", head, "--"]).await?;
    if snapshot != head {
        apply(worktree, snapshot).await?;
    }
    Ok(())
}

/// The paths of `snapshot` where the worktree has an ignored file, or lies
/// inside an ignored directory: restoring it would overwrite them, and no
/// checkpoint keeps ignored files.
pub async fn ignored_paths_in(worktree: &Path, snapshot: &str) -> Result<Vec<String>> {
    let ignored = run_isolated(
        worktree,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .await?;
    let ignored: Vec<&str> = ignored
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();
    if ignored.is_empty() {
        return Ok(Vec::new());
    }
    let paths = run_isolated(
        worktree,
        &["ls-tree", "-r", "-z", "--name-only", snapshot, "--"],
    )
    .await?;
    Ok(paths
        .split('\0')
        .filter(|path| {
            ignored
                .iter()
                .any(|ignored| match ignored.strip_suffix('/') {
                    Some(directory) => path.starts_with(&format!("{directory}/")),
                    None => path == ignored,
                })
        })
        .map(str::to_owned)
        .collect())
}

/// Point `reference` at `commit`, keeping it reachable.
pub async fn save(repo: &Path, reference: &str, commit: &str) -> Result<()> {
    run_isolated(repo, &["update-ref", reference, commit]).await?;
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
        assert_eq!(snapshot(&repo, "clean", true).await.unwrap(), head.trim());

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

        let commit = snapshot(&repo, "checkpoint", true).await.unwrap();
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

        assert!(
            same_files(
                &repo,
                &commit,
                &snapshot(&repo, "again", true).await.unwrap()
            )
            .await
            .unwrap()
        );
        assert!(!same_files(&repo, &commit, head.trim()).await.unwrap());

        let reference = reference("workspace", 2);
        save(&repo, &reference, &commit).await.unwrap();
        let saved = saved_refs(&repo).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].checkpoint, Some(("workspace".to_owned(), 2)));
        assert_eq!(saved[0].commit, commit);
        assert!(delete_at(&repo, &reference, &commit).await.unwrap());
        assert!(saved_refs(&repo).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rewind_replaces_head_and_unsaved_files_but_keeps_ignored_ones() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
        crate::test_support::commit(&repo, ".gitignore");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("tracked"), "checkpointed\n").unwrap();
        fs::write(repo.join("kept"), "kept\n").unwrap();
        let checkpoint = snapshot(&repo, "checkpoint", true).await.unwrap();
        let status = git(&repo, &["status", "--porcelain"]);

        fs::write(repo.join("later"), "later\n").unwrap();
        crate::test_support::commit(&repo, "later");
        fs::write(repo.join("tracked"), "later\n").unwrap();
        fs::remove_file(repo.join("kept")).unwrap();
        fs::write(repo.join("untracked"), "later\n").unwrap();
        fs::write(repo.join("ignored"), "build output\n").unwrap();

        rewind(&repo, head.trim(), &checkpoint).await.unwrap();
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        assert_eq!(
            fs::read_to_string(repo.join("tracked")).unwrap(),
            "checkpointed\n"
        );
        assert_eq!(fs::read_to_string(repo.join("kept")).unwrap(), "kept\n");
        assert!(!repo.join("untracked").exists());
        assert!(repo.join("ignored").exists());
    }

    #[tokio::test]
    async fn rewind_keeps_files_ignored_only_after_the_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("generated"), "checkpointed\n").unwrap();
        let checkpoint = snapshot(&repo, "checkpoint", true).await.unwrap();
        fs::remove_file(repo.join("generated")).unwrap();

        // A later commit ignores files the checkpoint's rules do not.
        fs::write(repo.join(".gitignore"), "build/\ngenerated\n").unwrap();
        crate::test_support::commit(&repo, ".gitignore");
        fs::create_dir(repo.join("build")).unwrap();
        fs::write(repo.join("build/out"), "output\n").unwrap();
        assert!(
            ignored_paths_in(&repo, &checkpoint)
                .await
                .unwrap()
                .is_empty()
        );
        fs::write(repo.join("generated"), "regenerated\n").unwrap();
        assert_eq!(
            ignored_paths_in(&repo, &checkpoint).await.unwrap(),
            ["generated"]
        );
        fs::remove_file(repo.join("generated")).unwrap();

        rewind(&repo, head.trim(), &checkpoint).await.unwrap();
        assert_eq!(
            fs::read_to_string(repo.join("build/out")).unwrap(),
            "output\n"
        );
        assert_eq!(
            fs::read_to_string(repo.join("generated")).unwrap(),
            "checkpointed\n"
        );
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
