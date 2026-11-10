use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The copy reports how many files it brought over, and none once they exist.
#[tokio::test]
async fn copy_ignored_counts_the_files_it_copies() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    );
    std::fs::write(repo.join(".env"), "KEY=1\n").unwrap();
    let config = root.path().join("worktrunk.toml");
    std::fs::write(&config, "").unwrap();
    let workspace = root.path().join("topic");
    crate::git::worktree::add(&repo, &workspace, "topic", Some("main"))
        .await
        .unwrap();
    assert_eq!(copy_ignored(&config, &workspace).await.unwrap(), 1);
    assert_eq!(copy_ignored(&config, &workspace).await.unwrap(), 0);
    assert_eq!(
        std::fs::read_to_string(workspace.join(".env")).unwrap(),
        "KEY=1\n"
    );
}
