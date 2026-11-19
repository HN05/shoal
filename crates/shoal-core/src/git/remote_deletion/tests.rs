use super::*;
use crate::test_support::{commit, git, repository};

#[tokio::test]
async fn a_branch_that_moved_after_its_check_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let work = repository(root.path(), "work");
    let remote = root.path().join("remote.git");
    let url = remote.to_str().unwrap();
    git(root.path(), &["init", "--quiet", "--bare", url]);
    let head = |repo: &Path, rev: &str| CommitId::from(git(repo, &["rev-parse", rev]).trim());
    git(&work, &["push", "--quiet", url, "main:topic"]);
    let checked = head(&work, "HEAD");
    // A push lands after the check.
    std::fs::write(work.join("later"), "later\n").unwrap();
    commit(&work, "later");
    git(&work, &["push", "--quiet", url, "main:topic"]);
    let pushed = head(&work, "HEAD");
    let login = GitLogin {
        username: "token",
        password: "secret".into(),
    };
    let topic = BranchName::from("topic");

    let refused = delete_at(url, &topic, &checked, &login).await.unwrap_err();
    assert!(refused.to_string().contains("moved"), "{refused:#}");
    assert_eq!(head(&remote, "refs/heads/topic"), pushed);
    delete_at(url, &topic, &pushed, &login).await.unwrap();
    assert_eq!(git(&remote, &["branch", "--list", "topic"]), "");
}
