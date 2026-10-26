use super::*;
use crate::test_support::{commit, git, repository};
use std::fs;

#[tokio::test]
async fn finds_files_of_one_name_at_any_depth() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    fs::create_dir_all(repo.join("app/App.xcodeproj")).unwrap();
    fs::write(repo.join("Package.resolved"), "root").unwrap();
    fs::write(repo.join("app/App.xcodeproj/Package.resolved"), "nested").unwrap();
    commit(&repo, "Package.resolved");
    commit(&repo, "app/App.xcodeproj/Package.resolved");
    fs::create_dir(repo.join("new")).unwrap();
    fs::write(repo.join("new/Package.resolved"), "untracked").unwrap();
    fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir(repo.join("ignored")).unwrap();
    fs::write(repo.join("ignored/Package.resolved"), "ignored").unwrap();

    let module = repository(root.path(), "module");
    fs::write(module.join("Package.resolved"), "module").unwrap();
    commit(&module, "Package.resolved");
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            module.to_str().unwrap(),
            "module",
        ],
    );

    let mut found = in_worktree(&repo, "Package.resolved").await.unwrap();
    found.sort();
    assert_eq!(
        found,
        [
            repo.join("Package.resolved"),
            repo.join("app/App.xcodeproj/Package.resolved"),
            repo.join("ignored/Package.resolved"),
            repo.join("module/Package.resolved"),
            repo.join("new/Package.resolved"),
        ]
    );
    let mut contents = at_revision(&repo, "HEAD", "Package.resolved")
        .await
        .unwrap();
    contents.sort();
    assert_eq!(contents, ["nested", "root"]);
}
