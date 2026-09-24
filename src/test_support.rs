//! Unit-test fixtures; keep their state and repositories under the returned root.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use crate::{daemon::workspace::Manager, paths::Paths};

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git in {} {args:?}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn commit(repo: &Path, file: &str) {
    git(repo, &["add", "--", file]);
    git(
        repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            file,
        ],
    );
}

pub fn repository(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Shoal Test"]);
    git(&repo, &["config", "user.email", "shoal@example.invalid"]);
    fs::write(repo.join("tracked"), "initial\n").unwrap();
    commit(&repo, "tracked");
    repo
}

/// Keep the temporary root alive until all manager operations have finished.
pub async fn manager() -> (tempfile::TempDir, Arc<Manager>) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let manager = Manager::open(Paths::for_test(root.path())).await.unwrap();
    (root, manager)
}
