use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::support;

/// The identity tests make commits, tags and rebases under.
const IDENTITY: [(&str, &str); 2] = [
    ("user.name", "Shoal Test"),
    ("user.email", "shoal@example.invalid"),
];

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
    git_with_identity(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

/// Run a Git command that records an author or committer, such as `commit`,
/// `tag -a` or `rebase`, under the tests' identity.
pub fn git_with_identity(repo: &Path, args: &[&str]) -> String {
    let options = IDENTITY.map(|(name, value)| format!("{name}={value}"));
    let identity = options.iter().flat_map(|option| ["-c", option.as_str()]);
    git(
        repo,
        &identity.chain(args.iter().copied()).collect::<Vec<_>>(),
    )
}

/// Give `repo` the tests' identity, for the commits Shoal itself makes there.
pub fn configure_identity(repo: &Path) {
    for (name, value) in IDENTITY {
        git(repo, &["config", name, value]);
    }
}
