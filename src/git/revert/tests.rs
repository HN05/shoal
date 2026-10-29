use super::*;
use crate::test_support::{git, repository};
use std::fs;

/// A repository whose `topic` branch adds `a` and then `b` to `main`.
fn with_topic() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["switch", "-c", "topic"]);
    for file in ["a", "b"] {
        fs::write(repo.join(file), "topic\n").unwrap();
        git(&repo, &["add", file]);
        git(
            &repo,
            &["commit", "-m", &format!("Add {file}\n\nWhy {file}")],
        );
    }
    git(&repo, &["switch", "main"]);
    (root, repo)
}

fn messages() -> Vec<String> {
    vec!["Add a\n\nWhy a\n".into(), "Add b\n\nWhy b\n".into()]
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

#[tokio::test]
async fn merge_commits_revert_against_their_first_parent() {
    let (_root, repo) = with_topic();
    git(&repo, &["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = Merge::of(&repo, &head(&repo), &messages()).await.unwrap();
    assert_eq!(merge, Merge::Commit(head(&repo)));
    let reverted = revert(&repo, &merge, "Revert \"Topic\"").await.unwrap();
    assert_eq!(reverted, Reverted::Committed);
    assert!(!repo.join("a").exists() && !repo.join("b").exists());
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s"]),
        "Revert \"Topic\"\n"
    );
    // Nothing is left to revert once it is undone.
    let error = revert(&repo, &merge, "Again").await.unwrap_err();
    assert!(error.to_string().contains("nothing to revert"), "{error:#}");
}

#[tokio::test]
async fn rebased_commits_revert_together_and_a_squash_alone() {
    let (_root, repo) = with_topic();
    git(&repo, &["merge", "--ff-only", "topic"]);
    let rebased = head(&repo);
    let merge = Merge::of(&repo, &rebased, &messages()).await.unwrap();
    let first = git(&repo, &["rev-parse", "HEAD~1"]).trim().to_owned();
    assert_eq!(merge, Merge::Commits(vec![rebased, first]));
    assert_eq!(
        revert(&repo, &merge, "Revert").await.unwrap(),
        Reverted::Committed
    );
    assert!(!repo.join("a").exists() && !repo.join("b").exists());

    // The same PR squashed onto the base it started from instead.
    git(&repo, &["reset", "--hard", "topic~2"]);
    git(&repo, &["merge", "--squash", "topic"]);
    git(&repo, &["commit", "-m", "Topic (#7)"]);
    let squashed = head(&repo);
    let merge = Merge::of(&repo, &squashed, &messages()).await.unwrap();
    assert_eq!(merge, Merge::Commits(vec![squashed]));
}

#[tokio::test]
async fn conflicting_reverts_report_their_paths_and_abort_cleanly() {
    let (_root, repo) = with_topic();
    git(&repo, &["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = Merge::of(&repo, &head(&repo), &messages()).await.unwrap();
    fs::write(repo.join("a"), "changed since\n").unwrap();
    git(&repo, &["commit", "-am", "Change a"]);
    let before = head(&repo);
    let reverted = revert(&repo, &merge, "Revert").await.unwrap();
    assert_eq!(reverted, Reverted::Conflicts(vec!["a".into()]));
    abort(&repo).await.unwrap();
    assert_eq!(head(&repo), before);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn messages_match_in_any_order_and_line_ending() {
    let listed = messages();
    assert!(same_messages(
        ["Add b\n\nWhy b", "Add a\n\nWhy a"].into_iter(),
        &listed
    ));
    assert!(!same_messages(
        ["Add a\n\nWhy a", "Topic (#7)"].into_iter(),
        &listed
    ));
}
