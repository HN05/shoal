//! Files of one name anywhere in a worktree or a revision.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::run;

/// Every file named `name` in the worktree, tracked, untracked or ignored,
/// since generated files are often ignored.
pub async fn in_worktree(worktree: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let listing = run(
        worktree,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--",
            &pathspec(name),
        ],
    )
    .await?;
    Ok(paths(&listing).map(|path| worktree.join(path)).collect())
}

/// The content of every file named `name` at `revision`.
pub async fn at_revision(repo: &Path, revision: &str, name: &str) -> Result<Vec<String>> {
    // `ls-tree` takes no glob pathspecs.
    let listing = run(repo, &["ls-tree", "-r", "-z", "--name-only", revision]).await?;
    let mut contents = Vec::new();
    for path in paths(&listing).filter(|path| Path::new(path).file_name() == Some(name.as_ref())) {
        contents.push(run(repo, &["cat-file", "blob", &format!("{revision}:{path}")]).await?);
    }
    Ok(contents)
}

fn pathspec(name: &str) -> String {
    format!(":(glob)**/{name}")
}

fn paths(listing: &str) -> impl Iterator<Item = &str> {
    // `ls-files` repeats a path for each unmerged stage.
    let mut seen = std::collections::HashSet::new();
    listing
        .split('\0')
        .filter(move |path| !path.is_empty() && seen.insert(*path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit, repository};
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

        let mut found = in_worktree(&repo, "Package.resolved").await.unwrap();
        found.sort();
        assert_eq!(
            found,
            [
                repo.join("Package.resolved"),
                repo.join("app/App.xcodeproj/Package.resolved"),
                repo.join("ignored/Package.resolved"),
                repo.join("new/Package.resolved"),
            ]
        );
        let mut contents = at_revision(&repo, "HEAD", "Package.resolved")
            .await
            .unwrap();
        contents.sort();
        assert_eq!(contents, ["nested", "root"]);
    }
}
