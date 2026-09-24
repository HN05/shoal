//! Filesystem policies used when claiming and verifying workspace ownership.
use crate::paths::Paths;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Whether a canonical candidate contains home or Shoal state, including equality.
/// Descendants of protected directories are left to the caller's policy.
pub(super) fn contains_protected_directory(path: &Path, paths: &Paths) -> Result<bool> {
    for protected in [&paths.home, &paths.state] {
        if fs::canonicalize(protected)?.starts_with(path) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Normalize an absolute path, resolving existing symlinks before `..`.
/// Missing components are retained and `..` is resolved lexically; dangling
/// symlinks and filesystem errors other than missing components are rejected.
pub(super) fn canonical_with_missing_tail(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "path must be absolute");
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => {
                result.push(other);
                match fs::symlink_metadata(&result) {
                    Ok(_) => result = fs::canonicalize(&result)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(result)
}

/// The path with its parent canonicalized, so a missing leaf still compares
/// against Git's absolute worktree records. The parent must exist; the leaf is
/// preserved even if it is a symlink.
pub(super) fn canonical_parent_only(path: &Path) -> Result<PathBuf> {
    let parent = fs::canonicalize(path.parent().context("missing workspace parent")?)?;
    Ok(parent.join(path.file_name().context("missing workspace name")?))
}

/// `device:inode` of a directory, stable across renames but not replacement.
fn device_inode(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", metadata.dev(), metadata.ino())
}

/// Follow symlinks to an existing directory and return its `device:inode`.
/// Missing paths and non-directories are errors.
pub(super) fn directory_device_inode(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)?;
    ensure!(metadata.is_dir(), "Git metadata is not a directory");
    Ok(device_inode(&metadata))
}

/// `device:inode` of a real, non-redirected directory; `None` when missing.
pub(super) fn real_directory_identity(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect repository directory"),
    };
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "repository path is not a directory or was replaced by a symlink"
    );
    ensure!(
        fs::canonicalize(path)? == path,
        "repository path was redirected; refusing deletion"
    );
    Ok(Some(device_inode(&metadata)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn missing_tail_normalizes_components_without_creating_them() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        fs::create_dir_all(root.join("target/child"))?;
        symlink(root.join("target/child"), root.join("alias"))?;
        for (input, expected) in [
            ("missing/leaf", "missing/leaf"),
            ("missing/../target/./child", "target/child"),
            ("alias/../new/leaf", "target/new/leaf"),
            ("alias/../child", "target/child"),
            ("missing/../alias/../new", "target/new"),
        ] {
            assert_eq!(
                canonical_with_missing_tail(&root.join(input))?,
                root.join(expected)
            );
        }
        assert!(!root.join("missing").exists());
        assert!(!root.join("target/new").exists());
        assert_eq!(
            canonical_with_missing_tail(Path::new("/../../"))?,
            Path::new("/")
        );
        assert!(canonical_with_missing_tail(Path::new("relative/path")).is_err());
        Ok(())
    }

    #[test]
    fn missing_tail_rejects_dangling_links_and_non_directory_ancestors() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        symlink(root.join("missing"), root.join("dangling"))?;
        fs::write(root.join("file"), "contents")?;
        for path in ["dangling", "dangling/leaf", "dangling/../leaf", "file/leaf"] {
            assert!(
                canonical_with_missing_tail(&root.join(path)).is_err(),
                "{path}"
            );
        }
        Ok(())
    }

    #[test]
    fn identity_policies_distinguish_missing_files_and_symlinks() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let directory = root.join("directory");
        fs::create_dir(&directory)?;
        let identity = directory_device_inode(&directory)?;
        assert_eq!(real_directory_identity(&directory)?, Some(identity.clone()));

        let alias = root.join("alias");
        symlink(&directory, &alias)?;
        assert_eq!(directory_device_inode(&alias)?, identity);
        assert!(real_directory_identity(&alias).is_err());
        fs::create_dir(directory.join("child"))?;
        assert!(real_directory_identity(&alias.join("child")).is_err());

        let missing = root.join("missing");
        assert!(directory_device_inode(&missing).is_err());
        assert_eq!(real_directory_identity(&missing)?, None);
        let dangling = root.join("dangling");
        symlink(&missing, &dangling)?;
        assert!(directory_device_inode(&dangling).is_err());
        assert!(real_directory_identity(&dangling).is_err());

        let file = root.join("file");
        fs::write(&file, "contents")?;
        assert!(directory_device_inode(&file).is_err());
        assert!(real_directory_identity(&file).is_err());
        Ok(())
    }

    #[test]
    fn identity_survives_rename_but_detects_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let directory = root.join("directory");
        fs::create_dir(&directory)?;
        let identity = directory_device_inode(&directory)?;
        let moved = root.join("moved");
        fs::rename(&directory, &moved)?;
        assert_eq!(directory_device_inode(&moved)?, identity);
        fs::create_dir(&directory)?;
        assert_ne!(real_directory_identity(&directory)?, Some(identity));
        Ok(())
    }

    #[test]
    fn parent_only_resolves_ancestors_but_preserves_the_leaf() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let alias = root.join("alias");
        symlink(&root, &alias)?;
        assert_eq!(
            canonical_parent_only(&alias.join("missing"))?,
            root.join("missing")
        );
        assert_eq!(canonical_parent_only(&alias)?, alias);
        assert!(canonical_parent_only(&root.join("missing/leaf")).is_err());
        Ok(())
    }

    #[test]
    fn protected_directories_reject_ancestors_and_resolve_aliases() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let state = root.join("state");
        fs::create_dir(&home)?;
        fs::create_dir(&state)?;
        let alias = root.join("alias");
        symlink(&home, &alias)?;
        let paths = Paths {
            home: alias,
            state: state.clone(),
            socket: state.join("daemon.sock"),
        };
        for path in [&root, &home, &state] {
            assert!(contains_protected_directory(path, &paths)?);
        }
        for path in [
            home.join("child"),
            state.join("child"),
            root.join("home-other"),
        ] {
            assert!(!contains_protected_directory(&path, &paths)?);
        }
        fs::remove_dir(&state)?;
        assert!(contains_protected_directory(&root.join("other"), &paths).is_err());
        Ok(())
    }
}
