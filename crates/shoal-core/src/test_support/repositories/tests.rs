use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use super::{commit, repository, repository_with};
use crate::test_support::git;

/// Whether Git reads `repo`'s worktree as clean, both after refreshing the
/// index and from the stat data the index holds.
fn assert_clean(repo: &Path) {
    assert_eq!(git(repo, &["diff-index", "--name-status", "HEAD"]), "");
    assert_eq!(git(repo, &["status", "--porcelain"]), "");
}

#[test]
fn a_built_repository_reads_as_git_would_build_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository_with(root.path(), "repo", &[("a", "one\n"), (".b", "two\n")]);
    assert_eq!(git(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/main\n");
    assert_eq!(
        git(&repo, &["log", "--format=%an <%ae> %cn <%ce>%n%B"]),
        "Shoal Test <shoal@example.invalid> Shoal Test <shoal@example.invalid>\ninitial\n\n"
    );
    assert_eq!(
        git(&repo, &["ls-tree", "-r", "--name-only", "HEAD"]),
        ".b\na\n"
    );
    assert_eq!(git(&repo, &["config", "maintenance.auto"]), "false\n");
    assert_clean(&repo);
}

#[test]
fn a_commit_records_what_git_add_would_stage() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::write(repo.join("script"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(repo.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
    commit(&repo, "script");
    std::os::unix::fs::symlink("script", repo.join("link")).unwrap();
    commit(&repo, "link");
    fs::create_dir(repo.join("dir")).unwrap();
    fs::write(repo.join("dir/nested"), "nested\n").unwrap();
    commit(&repo, "dir/nested");
    fs::remove_file(repo.join("tracked")).unwrap();
    commit(&repo, "tracked");

    let tree = git(
        &repo,
        &["ls-tree", "-r", "--format=%(objectmode) %(path)", "HEAD"],
    );
    assert_eq!(tree, "100644 dir/nested\n120000 link\n100755 script\n");
    assert_eq!(
        git(&repo, &["log", "--format=%s"]),
        "tracked\ndir/nested\nlink\nscript\ninitial\n"
    );
    assert_clean(&repo);
}

#[test]
fn a_commit_takes_what_the_index_already_holds() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::write(repo.join("staged"), "staged\n").unwrap();
    git(&repo, &["add", "staged"]);
    fs::write(repo.join("tracked"), "changed\n").unwrap();
    commit(&repo, "tracked");

    let changed = git(&repo, &["show", "--name-status", "--format=", "HEAD"]);
    assert_eq!(changed, "A\tstaged\nM\ttracked\n");
    assert_clean(&repo);
}

#[test]
fn a_commit_in_a_linked_worktree_moves_its_branch() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let linked = root.path().join("linked");
    git(
        &repo,
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    let main = git(&repo, &["rev-parse", "main"]);
    fs::write(linked.join("tracked"), "topic\n").unwrap();
    commit(&linked, "tracked");

    assert_eq!(git(&repo, &["rev-parse", "main"]), main);
    assert_eq!(
        git(&repo, &["log", "--format=%s", "-1", "topic"]),
        "tracked\n"
    );
    assert_clean(&linked);
    assert_clean(&repo);
}

#[test]
fn a_commit_of_a_directory_stages_it_whole() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::create_dir(repo.join("dir")).unwrap();
    fs::write(repo.join("dir/new"), "new\n").unwrap();
    fs::write(repo.join("tracked"), "changed\n").unwrap();
    commit(&repo, ".");

    let changed = git(&repo, &["show", "--name-status", "--format=", "HEAD"]);
    assert_eq!(changed, "A\tdir/new\nM\ttracked\n");
    assert_clean(&repo);
}

#[test]
fn a_commit_concludes_a_merge_in_progress() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["switch", "-q", "-c", "side"]);
    fs::write(repo.join("side"), "side\n").unwrap();
    commit(&repo, "side");
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    fs::write(repo.join("tracked"), "merging\n").unwrap();
    commit(&repo, "tracked");

    let parents = git(&repo, &["log", "-1", "--format=%p"]);
    assert_eq!(parents.split_whitespace().count(), 2, "{parents}");
    assert_clean(&repo);
}
