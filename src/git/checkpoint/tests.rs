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
        snapshot(&repo, "clean", true).await.unwrap().commit,
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

    let commit = snapshot(&repo, "checkpoint", true).await.unwrap().commit;
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
            &snapshot(&repo, "again", true).await.unwrap().commit
        )
        .await
        .unwrap()
    );
    assert!(!same_files(&repo, &commit, head.trim()).await.unwrap());

    let reference = reference(&commit);
    save(&repo, &reference, &commit).await.unwrap();
    assert_eq!(
        saved_refs(&repo).await.unwrap(),
        [(reference.clone(), commit.clone())]
    );
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
    let checkpoint = snapshot(&repo, "checkpoint", true).await.unwrap().commit;
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
    let checkpoint = snapshot(&repo, "checkpoint", true).await.unwrap().commit;
    fs::remove_file(repo.join("generated")).unwrap();

    // A later commit ignores files the checkpoint's rules do not.
    fs::write(repo.join(".gitignore"), "build/\ngenerated\n").unwrap();
    crate::test_support::commit(&repo, ".gitignore");
    fs::create_dir(repo.join("build")).unwrap();
    fs::write(repo.join("build/out"), "output\n").unwrap();
    let collisions = async || {
        ignored_collisions(&repo, head.trim(), &checkpoint)
            .await
            .unwrap()
    };
    assert!(collisions().await.is_empty());
    fs::write(repo.join("generated"), "regenerated\n").unwrap();
    assert_eq!(collisions().await, ["generated"]);
    fs::remove_file(repo.join("generated")).unwrap();
    // An ignored directory where the checkpoint has a file collides too.
    fs::create_dir(repo.join("generated")).unwrap();
    fs::write(repo.join("generated/cache"), "cache\n").unwrap();
    assert_eq!(collisions().await, ["generated"]);
    fs::remove_dir_all(repo.join("generated")).unwrap();

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
