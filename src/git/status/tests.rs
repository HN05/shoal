use std::{
    fs,
    time::{Duration, SystemTime},
};

use crate::test_support::{commit, git, repository};

#[tokio::test]
async fn unretained_work_finds_files_stashes_and_unpushed_commits() {
    let root = tempfile::tempdir().unwrap();
    let upstream = repository(root.path(), "upstream");
    let clone = root.path().join("clone");
    let (upstream_path, clone_path) = (upstream.to_str().unwrap(), clone.to_str().unwrap());
    git(root.path(), &["clone", "-q", upstream_path, clone_path]);
    // What the remote advertises now, not what tracking refs remember.
    let work = async || {
        let mut refs = crate::git::remote_heads(&clone, "origin").await.unwrap();
        refs.extend(crate::git::remote_tags(&clone, "origin").await.unwrap());
        let kept = crate::git::present_heads(&clone, &refs).await.unwrap();
        super::unretained_work(&clone, &kept).await.unwrap()
    };
    assert!(!work().await);
    fs::write(clone.join("tracked"), "changed\n").unwrap();
    assert!(work().await);
    let identity = ["-c", "user.name=T", "-c", "user.email=t@example.invalid"];
    git(&clone, &[&identity[..], &["stash", "-q"]].concat());
    assert!(work().await, "a stash");
    git(&clone, &["stash", "drop", "-q"]);
    assert!(!work().await);
    git(&clone, &["bisect", "start"]);
    assert!(work().await, "a bisect under way");
    git(&clone, &["bisect", "reset"]);
    assert!(!work().await);
    git(&clone, &["switch", "-q", "--detach"]);
    fs::write(clone.join("detached"), "detached\n").unwrap();
    commit(&clone, "detached");
    assert!(work().await, "a commit on a detached HEAD");
    git(&clone, &["tag", "kept"]);
    git(&clone, &["switch", "-q", "main"]);
    assert!(work().await, "a commit only a local tag has");
    git(&clone, &["push", "-q", "origin", "kept"]);
    assert!(!work().await, "a commit a remote tag has");
    git(&clone, &["switch", "-q", "--detach"]);
    fs::write(clone.join("annotated"), "annotated\n").unwrap();
    commit(&clone, "annotated");
    let tag = ["tag", "-a", "-m", "release", "release"];
    git(&clone, &[&identity[..], &tag].concat());
    git(&clone, &["switch", "-q", "main"]);
    assert!(work().await, "a commit only a local annotated tag has");
    git(&clone, &["push", "-q", "origin", "release"]);
    assert!(!work().await, "a commit a remote annotated tag has");
    git(&clone, &["switch", "-q", "-c", "local"]);
    fs::write(clone.join("new"), "new\n").unwrap();
    commit(&clone, "new");
    assert!(work().await, "a local commit");
    git(&clone, &["push", "-q", "origin", "local"]);
    assert!(!work().await, "a pushed commit");
    // A branch the remote deleted leaves a stale tracking ref behind.
    git(&upstream, &["branch", "-q", "-D", "local"]);
    assert!(work().await, "a commit only a stale tracking ref has");
}

#[tokio::test]
async fn status_leaves_a_stale_index_unwritten() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let tracked = fs::File::options()
        .write(true)
        .open(repo.join("tracked"))
        .unwrap();
    tracked
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    let index = fs::read(repo.join(".git/index")).unwrap();

    assert_eq!(super::porcelain(&repo).await.unwrap(), "");
    assert_eq!(super::changed_files(&repo).await.unwrap(), "");
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
}

#[tokio::test]
async fn merged_into_sees_merges_squashes_and_rebases() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    for file in ["one", "two"] {
        fs::write(repo.join(file), format!("{file}\n")).unwrap();
        commit(&repo, file);
    }
    let merged = async |target: &str| super::merged_into(&repo, "HEAD", target).await.unwrap();
    assert!(!merged("main").await);

    // Squash: one commit on main with the topic's combined changes,
    // followed by unrelated work.
    git(&repo, &["checkout", "-q", "-b", "squashed", "main"]);
    git(&repo, &["merge", "-q", "--squash", "topic"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    fs::write(repo.join("later"), "later\n").unwrap();
    commit(&repo, "later");
    // Rebase: equivalent patches on top of other work.
    git(&repo, &["checkout", "-q", "-b", "rebased", "main"]);
    fs::write(repo.join("other"), "other\n").unwrap();
    commit(&repo, "other");
    git(&repo, &["cherry-pick", "main..topic"]);
    git(&repo, &["checkout", "-q", "topic"]);
    assert!(merged("squashed").await);
    assert!(merged("rebased").await);
    assert!(merged("topic").await);

    // A merge whose resolution adds its own change is not proven by the
    // patches of its parents, which a rebase onto the target carries.
    git(&repo, &["checkout", "-q", "-b", "side", "main"]);
    fs::write(repo.join("side"), "side\n").unwrap();
    commit(&repo, "side");
    git(&repo, &["checkout", "-q", "-b", "merging", "topic"]);
    git(&repo, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    fs::write(repo.join("resolution"), "only in the merge\n").unwrap();
    git(&repo, &["add", "resolution"]);
    git(&repo, &["commit", "-q", "-m", "merge side"]);
    git(&repo, &["checkout", "-q", "-b", "picked", "main"]);
    git(&repo, &["cherry-pick", "main..topic", "side"]);
    git(&repo, &["checkout", "-q", "merging"]);
    assert!(!merged("picked").await);
    git(&repo, &["checkout", "-q", "topic"]);

    // A later commit is not on any of them.
    fs::write(repo.join("three"), "three\n").unwrap();
    commit(&repo, "three");
    for target in ["squashed", "rebased", "main"] {
        assert!(!merged(target).await, "{target}");
    }
}
