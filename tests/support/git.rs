use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::support;

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = support::isolated(repo, "git").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Create a repository under a test-owned directory with an initial commit.
pub fn init_repo(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir(&repo).unwrap();
    let repo = fs::canonicalize(repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    for (name, contents) in files {
        fs::write(repo.join(name), contents).unwrap();
    }
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    repo
}
