use std::{fs, path::Path};

use shoal_refs::CommitId;

use super::{Applied, rebase, squash};
use crate::test_support::{commit, git, repository};

fn rev(repo: &Path, revision: &str) -> CommitId {
    git(repo, &["rev-parse", revision]).trim().into()
}

/// What `git show` prints of `commit` with `format`.
fn show(repo: &Path, format: &str, commit: &str) -> String {
    git(repo, &["show", "-s", &format!("--format={format}"), commit])
}

/// `main` with a commit of its own after `side` forked from it, whose two
/// commits, to `feature` and `notes`, someone else made.
fn diverged(root: &Path) -> std::path::PathBuf {
    let repo = repository(root, "repo");
    git(&repo, &["switch", "-q", "-c", "side"]);
    for file in ["feature", "notes"] {
        fs::write(repo.join(file), format!("{file}\n")).unwrap();
        git(&repo, &["add", file]);
        let message = format!("Add {file}\n\nWhy {file}.");
        git(
            &repo,
            &[
                "-c",
                "user.name=Someone Else",
                "-c",
                "user.email=else@example.com",
                "commit",
                "-q",
                "-m",
                &message,
            ],
        );
    }
    git(&repo, &["switch", "-q", "main"]);
    fs::write(repo.join("main"), "main\n").unwrap();
    commit(&repo, "main");
    repo
}

/// The commit `applied` made, failing otherwise.
fn made(applied: Applied) -> CommitId {
    match applied {
        Applied::Commit(commit) => commit,
        other => panic!("made no commit: {other:?}"),
    }
}

#[tokio::test]
async fn a_squash_makes_one_commit_as_git_does_by_whoever_merges() {
    let root = tempfile::tempdir().unwrap();
    let repo = diverged(root.path());
    let (previous, source) = (rev(&repo, "main"), rev(&repo, "side"));
    let squashed = made(squash(&repo, &previous, &source, None).await.unwrap());
    git(&repo, &["switch", "-q", "--detach", "main"]);
    git(&repo, &["merge", "-q", "--squash", "side"]);
    git(&repo, &["commit", "-q", "--no-edit"]);
    let plain = rev(&repo, "HEAD");
    for format in ["%B", "%T", "%P", "%an <%ae>"] {
        assert_eq!(
            show(&repo, format, squashed.as_str()),
            show(&repo, format, plain.as_str()),
            "{format}"
        );
    }
    assert_eq!(
        show(&repo, "%an", squashed.as_str()),
        show(&repo, "%an", previous.as_str())
    );

    // A branch of one commit keeps its message, and a message given is
    // cleaned as git commit cleans it.
    let one = rev(&repo, "side~1");
    let kept = made(squash(&repo, &previous, &one, None).await.unwrap());
    assert_eq!(
        show(&repo, "%B", kept.as_str()),
        show(&repo, "%B", one.as_str())
    );
    let message = Some("Fix login  \n\n\nThe session expired early.\n\n");
    let given = made(squash(&repo, &previous, &source, message).await.unwrap());
    assert_eq!(
        show(&repo, "%B", given.as_str()).trim_end(),
        "Fix login\n\nThe session expired early."
    );
    // A default branch that has the branch's changes takes nothing more.
    assert_eq!(
        squash(&repo, &plain, &source, None).await.unwrap(),
        Applied::Unchanged
    );
}

#[tokio::test]
async fn a_rebase_copies_the_branch_s_commits_keeping_their_authors() {
    let root = tempfile::tempdir().unwrap();
    let repo = diverged(root.path());
    let (previous, source) = (rev(&repo, "main"), rev(&repo, "side"));
    let rebased = made(rebase(&repo, &previous, &source).await.unwrap());
    assert_eq!(rev(&repo, &format!("{rebased}~2")), previous);
    for (copy, original) in [
        (rebased.to_string(), "side"),
        (format!("{rebased}~1"), "side~1"),
    ] {
        assert_eq!(
            show(&repo, "%an%n%ad%n%B", &copy),
            show(&repo, "%an%n%ad%n%B", original)
        );
    }
    // The branch and the default branch stay where they were, and the
    // rebase's worktree goes.
    assert_eq!((rev(&repo, "side"), rev(&repo, "main")), (source, previous));
    let worktrees = git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(worktrees.matches("worktree ").count(), 1);
    assert_eq!(
        rebase(&repo, &rebased, &rev(&repo, "side")).await.unwrap(),
        Applied::Unchanged
    );
}

#[tokio::test]
async fn a_squash_or_rebase_that_conflicts_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = diverged(root.path());
    fs::write(repo.join("feature"), "main's own\n").unwrap();
    commit(&repo, "feature");
    let (previous, source) = (rev(&repo, "main"), rev(&repo, "side"));
    let Applied::Conflicts(report) = squash(&repo, &previous, &source, None).await.unwrap() else {
        panic!("the squash applied");
    };
    assert!(report.contains("feature"), "{report}");
    let Applied::Conflicts(report) = rebase(&repo, &previous, &source).await.unwrap() else {
        panic!("the rebase applied");
    };
    assert!(report.contains("feature"), "{report}");
    assert_eq!((rev(&repo, "side"), rev(&repo, "main")), (source, previous));
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    let worktrees = git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(worktrees.matches("worktree ").count(), 1);
}
