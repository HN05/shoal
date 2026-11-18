//! Repositories the tests build, with the commits they make there.
use std::{
    fs,
    path::{Path, PathBuf},
};

use super::git;

mod in_process;
#[cfg(test)]
mod tests;

/// The identity tests make commits, tags and rebases under.
const AUTHOR: [(&str, &str); 2] = [
    ("user.name", "Shoal Test"),
    ("user.email", "shoal@example.invalid"),
];

/// Run a Git command that records an author or committer, such as `commit`,
/// `tag -a` or `rebase`, under the tests' identity, in a repository that
/// [`repository_with`] did not create, such as a clone.
pub fn git_with_identity(repo: &Path, args: &[&str]) -> String {
    let options = AUTHOR.map(|(name, value)| format!("{name}={value}"));
    let identity = options.iter().flat_map(|option| ["-c", option.as_str()]);
    git(
        repo,
        &identity.chain(args.iter().copied()).collect::<Vec<_>>(),
    )
}

/// Commit `file` in `repo`, with its name as the message, and whatever
/// else the index holds, as `git add` and `git commit` would: in this
/// process, or through Git where Git would do more, such as for a directory.
pub fn commit(repo: &Path, file: &str) {
    if !in_process::commit(repo, &[file], file, identity()) {
        git(repo, &["add", "--", file]);
        git_with_identity(repo, &["commit", "-m", file]);
    }
}

/// The tests' name and email, as [`AUTHOR`] sets them.
fn identity() -> [&'static str; 2] {
    AUTHOR.map(|(_, value)| value)
}

/// Set each of `entries`, a name such as `user.name` with its value, in the
/// config of the repository `repo` created, in one write rather than a
/// `git config` each, since tests create many repositories. A value must
/// read as written, without quotes, backslashes or comment characters.
pub fn set_config(repo: &Path, entries: &[(&str, &str)]) {
    use std::io::Write;
    let mut text = String::new();
    for (name, value) in entries {
        assert!(
            !value.contains(['"', '\\', '#', ';', '\n']),
            "{name} = {value}"
        );
        let (section, key) = name.rsplit_once('.').unwrap();
        text.push_str(&format!("[{section}]\n\t{key} = {value}\n"));
    }
    let config = repo.join(".git/config");
    let mut file = fs::OpenOptions::new().append(true).open(config).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// [`repository_with`] the file `tracked`.
pub fn repository(root: &Path, name: &str) -> PathBuf {
    repository_with(root, name, &[("tracked", "initial\n")])
}

/// Turns off the maintenance Git starts after commands such as `commit`,
/// which runs detached and may still write under `.git` once the command
/// has returned, while a test reads or removes the repository.
const NO_AUTO_MAINTENANCE: (&str, &str) = ("maintenance.auto", "false");

/// Create the repository `name` in `root`, at its canonical path, on `main`
/// with the tests' identity, for the commits Shoal makes there too, without
/// automatic maintenance, and an initial commit of `files`.
pub fn repository_with(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo = configured_repository(root, name, in_process::init);
    for (name, contents) in files {
        fs::write(repo.join(name), contents).unwrap();
    }
    let names: Vec<_> = files.iter().map(|(name, _)| *name).collect();
    assert!(in_process::commit(&repo, &names, "initial", identity()));
    repo
}

/// The repository [`repository`] creates, with its refs in a reftable,
/// which Git reads and gix does not, so Git builds it.
pub fn reftable_repository(root: &Path, name: &str) -> PathBuf {
    let repo = configured_repository(root, name, |repo| {
        git(repo, &["init", "-b", "main", "--ref-format=reftable"]);
    });
    fs::write(repo.join("tracked"), "initial\n").unwrap();
    git(&repo, &["add", "tracked"]);
    git(&repo, &["commit", "-m", "initial"]);
    repo
}

/// The repository `name` in `root`, at its canonical path, that `init`
/// creates, with the configuration [`repository_with`] describes.
fn configured_repository(root: &Path, name: &str, init: impl FnOnce(&Path)) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir(&repo).unwrap();
    let repo = fs::canonicalize(repo).unwrap();
    init(&repo);
    set_config(&repo, &[AUTHOR[0], AUTHOR[1], NO_AUTO_MAINTENANCE]);
    repo
}
