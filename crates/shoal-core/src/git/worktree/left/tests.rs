use super::*;

async fn git(dir: &Path, args: &[&str]) -> String {
    crate::git::run(dir, args).await.unwrap()
}

/// A repository whose worktree on `topic`, at `<root>/topic`, Git disowned
/// after deleting its `.git` link, `.gitignore` and `b.txt`, leaving
/// `src/a.txt` changed, an ignored `target/` and an untracked `notes.md`.
/// Returns the repository, the directory and the commit it was on.
async fn disowned(root: &Path) -> (PathBuf, PathBuf, CommitId) {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-b", "main"]).await;
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(repo.join("b.txt"), "b\n").unwrap();
    std::fs::write(repo.join("src/a.txt"), "a\n").unwrap();
    git(&repo, &["add", "--all"]).await;
    let identity = [
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
    ];
    git(
        &repo,
        &[&identity[..], &["commit", "-m", "initial"]].concat(),
    )
    .await;
    let dir = root.join("topic");
    let path = dir.to_str().unwrap();
    git(&repo, &["worktree", "add", "-b", "topic", path]).await;
    let head = git(&repo, &["rev-parse", "topic"]).await.trim().into();
    let admin_dir = git(&dir, &["rev-parse", "--absolute-git-dir"]).await;
    std::fs::remove_dir_all(admin_dir.trim_end()).unwrap();
    for file in [".git", ".gitignore", "b.txt"] {
        std::fs::remove_file(dir.join(file)).unwrap();
    }
    std::fs::write(dir.join("src/a.txt"), "changed\n").unwrap();
    std::fs::write(dir.join("notes.md"), "notes\n").unwrap();
    std::fs::create_dir(dir.join("target")).unwrap();
    std::fs::write(dir.join("target/build"), "built\n").unwrap();
    (repo, dir, head)
}

/// Files Git deleted count as unchanged, and the commit's ignore rules
/// apply though their file is gone.
#[tokio::test]
async fn the_files_left_differ_from_the_commit_by_what_changed_or_is_new() {
    let root = tempfile::tempdir().unwrap();
    let (repo, dir, head) = disowned(root.path()).await;
    let left = LeftFiles::read(&repo, &dir, &head).await.unwrap();
    assert_eq!(left.status().await.unwrap(), " M src/a.txt\n?? notes.md\n");
    let bytes = "changed\n".len() + "notes\n".len();
    assert_eq!(left.unsaved_bytes().await.unwrap(), Some(bytes as u64));
}

/// The snapshot keeps the commit's files, with those left laid over them.
#[tokio::test]
async fn a_snapshot_lays_the_files_left_over_the_commit() {
    let root = tempfile::tempdir().unwrap();
    let (repo, dir, head) = disowned(root.path()).await;
    let left = LeftFiles::read(&repo, &dir, &head).await.unwrap();
    let snapshot = left.snapshot("checkpoint").await.unwrap();
    let parent = git(&repo, &["rev-parse", &format!("{snapshot}^")]).await;
    assert_eq!(parent.trim(), head.as_str());
    let files = git(&repo, &["ls-tree", "-r", "--name-only", snapshot.as_str()]).await;
    assert_eq!(files, ".gitignore\nb.txt\nnotes.md\nsrc/a.txt\n");
    let changed = git(&repo, &["show", &format!("{snapshot}:src/a.txt")]).await;
    assert_eq!(changed, "changed\n");
}

#[tokio::test]
async fn nothing_left_that_differs_snapshots_as_the_commit() {
    let root = tempfile::tempdir().unwrap();
    let (repo, dir, head) = disowned(root.path()).await;
    std::fs::remove_file(dir.join("notes.md")).unwrap();
    std::fs::write(dir.join("src/a.txt"), "a\n").unwrap();
    let left = LeftFiles::read(&repo, &dir, &head).await.unwrap();
    assert_eq!(left.status().await.unwrap(), "");
    assert_eq!(left.snapshot("checkpoint").await.unwrap(), head);
}

/// Paths are quoted as Git's status quotes them, so a name with a newline
/// stays one entry.
#[tokio::test]
async fn the_status_quotes_paths() {
    let root = tempfile::tempdir().unwrap();
    let (repo, dir, head) = disowned(root.path()).await;
    std::fs::remove_file(dir.join("notes.md")).unwrap();
    std::fs::write(dir.join("two\nlines"), "notes\n").unwrap();
    let left = LeftFiles::read(&repo, &dir, &head).await.unwrap();
    let status = left.status().await.unwrap();
    assert_eq!(status, " M src/a.txt\n?? \"two\\nlines\"\n");
}

/// A snapshot records only a submodule's commit, so its changed files
/// cannot be kept.
#[tokio::test]
async fn a_changed_submodule_cannot_be_kept() {
    let root = tempfile::tempdir().unwrap();
    let (repo, dir, _) = disowned(root.path()).await;
    let identity = [
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
    ];
    let commit = |dir: PathBuf| async move {
        let args = [&identity[..], &["commit", "--allow-empty", "-m", "commit"]].concat();
        git(&dir, &args).await;
        git(&dir, &["rev-parse", "HEAD"]).await.trim().to_owned()
    };
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    git(&sub, &["init", "-b", "main"]).await;
    let recorded = commit(sub.clone()).await;
    let gitlink = format!("160000,{recorded},sub");
    git(&repo, &["update-index", "--add", "--cacheinfo", &gitlink]).await;
    let head: CommitId = commit(repo.clone()).await.as_str().into();
    std::fs::write(sub.join("unsaved"), "work\n").unwrap();
    let left = LeftFiles::read(&repo, &dir, &head).await.unwrap();
    assert_eq!(left.unsaved_bytes().await.unwrap(), None);
}
