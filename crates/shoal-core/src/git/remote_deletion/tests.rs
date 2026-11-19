use super::*;
use crate::test_support::{git, git_with_identity};

/// Push `topic` to a remote in the object format `format`, move it there
/// after its check, and delete it at the checked commit, which keeps it,
/// then at the one it moved to, which deletes it.
async fn moved_after_its_check(format: &str) {
    let root = tempfile::tempdir().unwrap();
    let (work, remote) = (root.path().join("work"), root.path().join("remote.git"));
    let (work_path, path) = (work.to_str().unwrap(), remote.to_str().unwrap());
    let url = &format!("file://{path}");
    let object_format = format!("--object-format={format}");
    git(root.path(), &["init", "--quiet", &object_format, work_path]);
    git(
        root.path(),
        &["init", "--quiet", "--bare", &object_format, path],
    );
    let push = |file: &str| {
        std::fs::write(work.join(file), "text\n").unwrap();
        git(&work, &["add", file]);
        git_with_identity(&work, &["commit", "--quiet", "-m", file]);
        git(&work, &["push", "--quiet", url, "HEAD:refs/heads/topic"]);
        CommitId::from(git(&work, &["rev-parse", "HEAD"]).trim())
    };
    let checked = push("first");
    // A push lands after the check.
    let pushed = push("later");
    let login = GitLogin {
        username: "token",
        password: "secret".into(),
    };
    let topic = BranchName::from("topic");

    let refused = delete_at(url, &topic, &checked, &login).await.unwrap_err();
    assert!(refused.to_string().contains("moved"), "{refused:#}");
    let kept = git(&remote, &["rev-parse", "refs/heads/topic"]);
    assert_eq!(CommitId::from(kept.trim()), pushed);
    delete_at(url, &topic, &pushed, &login).await.unwrap();
    assert_eq!(git(&remote, &["branch", "--list", "topic"]), "");
}

#[tokio::test]
async fn a_sha1_branch_that_moved_after_its_check_is_kept() {
    moved_after_its_check("sha1").await;
}

#[tokio::test]
async fn a_sha256_branch_that_moved_after_its_check_is_kept() {
    moved_after_its_check("sha256").await;
}
