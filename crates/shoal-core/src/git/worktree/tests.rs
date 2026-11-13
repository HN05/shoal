use super::*;

async fn git(repo: &Path, args: &[&str]) -> String {
    crate::git::run(repo, args).await.unwrap()
}

/// A repository of `format` with one commit on `main`.
fn repository(root: &Path, format: &str) -> std::path::PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "-b", "main", &format!("--object-format={format}")][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
    repo
}

/// Open `name`, a new branch or an existing one, in a new repository of
/// `format`, and check that the worktree is on the branch of that literal
/// name.
async fn check_literal_branch(format: &str, name: &str, existing: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), format);
    if existing {
        git(&repo, &["branch", "--", name, "main"]).await;
    }
    let workspace = root.path().join("workspace");
    let base = (!existing).then_some("main");
    add(&repo, &workspace, &name.into(), base).await.unwrap();
    assert_eq!(
        git(&workspace, &["symbolic-ref", "HEAD"]).await,
        format!("refs/heads/{name}\n")
    );
}

/// Names around the reserved ones that Git takes literally, each opened new
/// and existing in both object formats: hex names one digit off an object
/// ID's length, non-hex names of that length, and reserved names as one
/// component of several.
macro_rules! literal_branch_tests {
    ($($case:ident: $name:expr;)*) => {$(
        mod $case {
            use super::*;

            #[tokio::test]
            async fn new_in_sha1() {
                check_literal_branch("sha1", &$name, false).await;
            }

            #[tokio::test]
            async fn existing_in_sha1() {
                check_literal_branch("sha1", &$name, true).await;
            }

            #[tokio::test]
            async fn new_in_sha256() {
                check_literal_branch("sha256", &$name, false).await;
            }

            #[tokio::test]
            async fn existing_in_sha256() {
                check_literal_branch("sha256", &$name, true).await;
            }
        }
    )*};
}

literal_branch_tests! {
    hex_39_digits: "a".repeat(39);
    hex_41_digits: "a".repeat(41);
    hex_63_digits: "a".repeat(63);
    hex_65_digits: "a".repeat(65);
    non_hex_40_digits: "g".repeat(40);
    non_hex_64_digits: "g".repeat(64);
    head_as_leaf: "topic/HEAD";
    head_as_parent: "HEAD/topic";
    at_as_leaf: "topic/@";
    at_as_parent: "@/topic";
    object_id_as_leaf: format!("topic/{}", "b".repeat(40));
    object_id_as_parent: format!("{}/topic", "b".repeat(40));
}

/// Template syntax in the workspace path stays literal, and the default
/// branch opens there while the main checkout is on another branch.
#[tokio::test]
async fn the_default_branch_opens_at_a_literal_path() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "sha1");
    git(&repo, &["switch", "-c", "other"]).await;
    let workspace = root.path().join("main {{ branch }}");
    add(&repo, &workspace, &"main".into(), None).await.unwrap();
    assert_eq!(
        git(&workspace, &["symbolic-ref", "HEAD"]).await,
        "refs/heads/main\n"
    );
}

#[tokio::test]
async fn a_new_branch_that_exists_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "sha1");
    git(&repo, &["branch", "topic"]).await;
    let workspace = root.path().join("topic");
    assert!(
        add(&repo, &workspace, &"topic".into(), Some("main"))
            .await
            .is_err()
    );
}

/// A repository with a worktree on `topic` at `<root>/topic`.
async fn with_topic(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = repository(root, "sha1");
    let workspace = root.join("topic");
    add(&repo, &workspace, &"topic".into(), Some("main"))
        .await
        .unwrap();
    (repo, workspace)
}

async fn has_branch(repo: &Path, name: &str) -> bool {
    crate::git::ref_exists(
        repo,
        &crate::git::local_ref(&name.into()),
        crate::git::command,
    )
    .await
    .unwrap()
}

/// Remove a worktree that was switched from `topic` to `moved` with
/// `branch`, and check the outcome and which branches remain.
async fn check_branch_removal(branch: BranchRemoval, outcome: BranchOutcome, kept: bool) {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace) = with_topic(root.path()).await;
    // The worktree's own branch goes, whichever it was created on.
    git(&workspace, &["switch", "-c", "moved"]).await;
    let removed = remove(&repo, &workspace, FileRemoval::CleanOnly, branch)
        .await
        .unwrap();
    assert!(!workspace.exists());
    assert_eq!(removed.branch.as_deref(), Some("moved"));
    assert_eq!(removed.branch_outcome, outcome);
    assert_eq!(has_branch(&repo, "moved").await, kept);
    assert!(has_branch(&repo, "topic").await);
}

#[tokio::test]
async fn removal_keeps_the_branch_unless_asked() {
    check_branch_removal(BranchRemoval::Keep, BranchOutcome::NotAttempted, true).await;
}

#[tokio::test]
async fn removal_deletes_the_branch_when_asked() {
    check_branch_removal(BranchRemoval::Delete, BranchOutcome::Deleted, false).await;
}

#[tokio::test]
async fn a_detached_worktree_has_no_branch_to_delete() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace) = with_topic(root.path()).await;
    git(&workspace, &["switch", "--detach"]).await;
    let removed = remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Delete,
    )
    .await
    .unwrap();
    assert_eq!(removed.branch, None);
    assert_eq!(removed.branch_outcome, BranchOutcome::NotAttempted);
    assert!(has_branch(&repo, "topic").await);
}

#[tokio::test]
async fn a_branch_checked_out_elsewhere_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace) = with_topic(root.path()).await;
    // A second worktree on the same branch, which Git allows only by force.
    let other = root.path().join("other");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--force",
            other.to_str().unwrap(),
            "topic",
        ],
    )
    .await;
    let removed = remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Delete,
    )
    .await
    .unwrap();
    assert!(!workspace.exists());
    assert_eq!(removed.branch_outcome, BranchOutcome::RetainedCheckedOut);
    assert!(has_branch(&repo, "topic").await);
}

#[tokio::test]
async fn only_forced_removal_deletes_changed_files() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace) = with_topic(root.path()).await;
    std::fs::write(workspace.join("untracked"), "notes").unwrap();
    let refused = remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Delete,
    )
    .await;
    assert!(refused.is_err());
    assert!(workspace.join("untracked").exists());
    assert!(has_branch(&repo, "topic").await);
    let removed = remove(&repo, &workspace, FileRemoval::Force, BranchRemoval::Delete)
        .await
        .unwrap();
    assert!(!workspace.exists());
    assert_eq!(removed.branch_outcome, BranchOutcome::Deleted);
}

/// A worktree whose directory is gone is forgotten, its branch reported.
#[tokio::test]
async fn a_deleted_worktree_is_unregistered() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace) = with_topic(root.path()).await;
    std::fs::remove_dir_all(&workspace).unwrap();
    let removed = remove(&repo, &workspace, FileRemoval::Force, BranchRemoval::Keep)
        .await
        .unwrap();
    assert_eq!(removed.branch.as_deref(), Some("topic"));
    let listed = git(&repo, &["worktree", "list", "--porcelain"]).await;
    assert!(!listed.contains("topic"), "{listed}");
}

/// A clean worktree with a checked-out submodule, which Git removes only by
/// force, goes without one; a change in it still keeps it.
#[tokio::test]
async fn a_clean_worktree_with_a_submodule_is_removed() {
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    std::fs::create_dir(&library_root).unwrap();
    let library = repository(&library_root, "sha1");
    let repo = repository(root.path(), "sha1");
    let file = ["-c", "protocol.file.allow=always"];
    let library = library.to_str().unwrap();
    git(
        &repo,
        &[&file[..], &["submodule", "add", library, "lib"]].concat(),
    )
    .await;
    let identity = [
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
    ];
    git(
        &repo,
        &[&identity[..], &["commit", "-m", "Add lib"]].concat(),
    )
    .await;
    let workspace = root.path().join("topic");
    add(&repo, &workspace, &"topic".into(), Some("main"))
        .await
        .unwrap();
    git(
        &workspace,
        &[&file[..], &["submodule", "update", "--init"]].concat(),
    )
    .await;
    std::fs::write(workspace.join("lib").join("scratch"), "notes").unwrap();
    let refused = remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Keep,
    )
    .await;
    assert!(refused.is_err());
    assert!(workspace.join("lib/scratch").exists());
    std::fs::remove_file(workspace.join("lib/scratch")).unwrap();
    remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Keep,
    )
    .await
    .unwrap();
    assert!(!workspace.exists());
    // Git still refuses once the submodule is deinitialized, since its
    // repository stays in the worktree's Git directory.
    add(&repo, &workspace, &"deinit".into(), Some("main"))
        .await
        .unwrap();
    git(
        &workspace,
        &[&file[..], &["submodule", "update", "--init"]].concat(),
    )
    .await;
    git(&workspace, &["submodule", "deinit", "--all"]).await;
    remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Keep,
    )
    .await
    .unwrap();
    assert!(!workspace.exists());
}
