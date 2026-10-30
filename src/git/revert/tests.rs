use super::*;
use crate::test_support::{git, isolated_test, repository};
use std::fs;

/// The production Git calls use the caller's configuration and hooks, so the
/// cases run in a process without the user's.
#[tokio::test]
async fn reverts_run_without_the_users_git_configuration() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        isolated_test(root.path(), "git::revert::tests::revert_cases").output(),
    )
    .await
    .expect("the revert cases stalled")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[tokio::test]
#[ignore = "isolated environment launched by reverts_run_without_the_users_git_configuration"]
async fn revert_cases() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    merge_commits_revert_against_their_first_parent().await;
    rebased_commits_revert_together_and_a_squash_alone().await;
    a_matching_message_does_not_make_a_base_commit_part_of_the_pr().await;
    merges_of_a_squashed_head_revert_and_imported_merges_refuse().await;
    a_partial_copy_of_the_pr_is_refused().await;
    missing_pr_commits_are_fetched_from_the_head_ref().await;
    conflicting_reverts_report_their_paths_and_abort_cleanly().await;
    changes_to_tracked_files_refuse_the_revert().await;
}

/// A repository whose `topic` branch adds `a` and then `b` to `main`, which
/// has since gained `base`.
fn with_topic() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["switch", "-c", "topic"]);
    for file in ["a", "b"] {
        add(&repo, file, &format!("Add {file}"));
    }
    git(&repo, &["switch", "main"]);
    add(&repo, "base", "Add base");
    (root, repo)
}

fn add(repo: &Path, file: &str, message: &str) {
    fs::write(repo.join(file), format!("{file}\n")).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-m", message]);
}

/// The topic's commits, oldest first, as the forge lists them.
fn topic_commits(repo: &Path) -> Vec<String> {
    git(repo, &["rev-list", "--reverse", "main..topic"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn merged<'a>(merge_commit: &'a str, commits: &'a [String]) -> Merged<'a> {
    Merged {
        merge_commit,
        head: commits.last().map_or("", String::as_str),
        commits,
        head_ref: None,
    }
}

/// Run each command in `repo`, as the workspace's tracked commands run.
async fn run_in(repo: &Path, command: Vec<OsString>) -> Result<i32> {
    let status = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(repo)
        .status()
        .await?;
    Ok(status.code().unwrap_or(-1))
}

async fn reverted(repo: &Path, merge: &Merge, message: &str) -> Result<Reverted> {
    revert(repo, merge, message, async |command| {
        run_in(repo, command).await
    })
    .await
}

async fn merge_commits_revert_against_their_first_parent() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    git(&repo, &["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = Merge::of(&repo, &merged(&head(&repo), &commits))
        .await
        .unwrap();
    assert_eq!(merge, Merge::Commit(head(&repo)));
    let outcome = reverted(&repo, &merge, "Revert \"Topic\"").await.unwrap();
    assert_eq!(outcome, Reverted::Committed);
    assert!(!repo.join("a").exists() && !repo.join("b").exists());
    assert!(repo.join("base").exists());
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s"]),
        "Revert \"Topic\"\n"
    );
    // Nothing is left to revert once it is undone.
    let error = reverted(&repo, &merge, "Again").await.unwrap_err();
    assert!(error.to_string().contains("nothing to revert"), "{error:#}");
}

async fn rebased_commits_revert_together_and_a_squash_alone() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    let before = head(&repo);
    git(&repo, &["cherry-pick", "main..topic"]);
    let rebased = head(&repo);
    let merge = Merge::of(&repo, &merged(&rebased, &commits)).await.unwrap();
    let first = git(&repo, &["rev-parse", "HEAD~1"]).trim().to_owned();
    assert_eq!(merge, Merge::Commits(vec![rebased, first]));
    assert_eq!(
        reverted(&repo, &merge, "Revert").await.unwrap(),
        Reverted::Committed
    );
    assert!(!repo.join("a").exists() && !repo.join("b").exists());

    // The same PR squashed onto the base instead.
    git(&repo, &["reset", "--hard", &before]);
    git(&repo, &["merge", "--squash", "topic"]);
    git(&repo, &["commit", "-m", "Topic (#7)"]);
    let squashed = head(&repo);
    let merge = Merge::of(&repo, &merged(&squashed, &commits))
        .await
        .unwrap();
    assert_eq!(merge, Merge::Commits(vec![squashed]));
}

async fn a_matching_message_does_not_make_a_base_commit_part_of_the_pr() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    // An unrelated base commit carries the first PR commit's message, and
    // the squash the second's.
    add(&repo, "unrelated", "Add a");
    git(&repo, &["merge", "--squash", "topic"]);
    git(&repo, &["commit", "-m", "Add b"]);
    let squashed = head(&repo);
    let merge = Merge::of(&repo, &merged(&squashed, &commits))
        .await
        .unwrap();
    assert_eq!(merge, Merge::Commits(vec![squashed]));
}

async fn merges_of_a_squashed_head_revert_and_imported_merges_refuse() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    // GitLab squashes the source branch, then merges the squash.
    git(&repo, &["switch", "-c", "squashed", "main~1"]);
    git(&repo, &["merge", "--squash", "topic"]);
    git(&repo, &["commit", "-m", "Topic"]);
    git(&repo, &["switch", "main"]);
    git(
        &repo,
        &["merge", "--no-ff", "-m", "Merge topic", "squashed"],
    );
    let merge = Merge::of(&repo, &merged(&head(&repo), &commits))
        .await
        .unwrap();
    assert_eq!(merge, Merge::Commit(head(&repo)));

    // A source branch that merged main, then fast-forwarded main: its tip
    // merges main's commits, not the PR's.
    git(&repo, &["reset", "--hard", "HEAD~1"]);
    git(&repo, &["switch", "topic"]);
    git(&repo, &["merge", "--no-ff", "-m", "Merge main", "main"]);
    let tip = head(&repo);
    git(&repo, &["switch", "main"]);
    git(&repo, &["merge", "--ff-only", "topic"]);
    let facts = Merged {
        head: &tip,
        ..merged(&tip, &commits)
    };
    let error = Merge::of(&repo, &facts).await.unwrap_err();
    assert!(error.to_string().contains("by hand"), "{error:#}");
}

async fn a_partial_copy_of_the_pr_is_refused() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    // Only the PR's last commit reached the base.
    git(&repo, &["cherry-pick", "topic"]);
    let error = Merge::of(&repo, &merged(&head(&repo), &commits))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("by hand"), "{error:#}");
}

async fn missing_pr_commits_are_fetched_from_the_head_ref() {
    let (root, origin) = with_topic();
    let commits = topic_commits(&origin);
    git(&origin, &["cherry-pick", "main..topic"]);
    git(&origin, &["update-ref", "refs/pull/7/head", "topic"]);
    git(&origin, &["branch", "-D", "topic"]);
    let local = root.path().join("local");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            origin.to_str().unwrap(),
            "local",
        ],
    );
    let rebased = head(&local);
    assert!(!git::has_commit(&local, &commits[0]).await.unwrap());
    let error = Merge::of(&local, &merged(&rebased, &commits))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keeps no ref"), "{error:#}");

    let facts = Merged {
        head_ref: Some("refs/pull/7/head"),
        ..merged(&rebased, &commits)
    };
    let merge = Merge::of(&local, &facts).await.unwrap();
    let first = git(&local, &["rev-parse", "HEAD~1"]).trim().to_owned();
    assert_eq!(merge, Merge::Commits(vec![rebased, first]));
    // The temporary ref it fetched into is gone.
    assert_eq!(git(&local, &["for-each-ref", "refs/shoal"]), "");
}

async fn conflicting_reverts_report_their_paths_and_abort_cleanly() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    git(&repo, &["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = Merge::of(&repo, &merged(&head(&repo), &commits))
        .await
        .unwrap();
    fs::write(repo.join("a"), "changed since\n").unwrap();
    git(&repo, &["commit", "-am", "Change a"]);
    let before = head(&repo);
    let outcome = reverted(&repo, &merge, "Revert").await.unwrap();
    assert_eq!(outcome, Reverted::Conflicts(vec!["a".into()]));
    assert_eq!(head(&repo), before);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

async fn changes_to_tracked_files_refuse_the_revert() {
    let (_root, repo) = with_topic();
    let commits = topic_commits(&repo);
    git(&repo, &["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = Merge::of(&repo, &merged(&head(&repo), &commits))
        .await
        .unwrap();
    // As a setup that formats or regenerates a tracked file might leave it.
    fs::write(repo.join("tracked"), "set up\n").unwrap();
    git(&repo, &["add", "tracked"]);
    let error = reverted(&repo, &merge, "Revert").await.unwrap_err();
    assert!(error.to_string().contains("tracked files"), "{error:#}");
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]), "Merge topic\n");
}

#[test]
fn patch_ids_sort_whatever_order_git_lists_them_in() {
    let output = "fff 111\naaa 222\n";
    assert_eq!(sorted_patch_ids(output), ["aaa", "fff"]);
    assert!(sorted_patch_ids("").is_empty());
}
