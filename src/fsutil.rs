//! Small filesystem operations shared by the CLI and daemon.
use std::{
    ffi::OsStr,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::Context;

/// Read HOME without imposing caller-specific path validation.
pub fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// Expand a leading `~` path component, without resolving users or symlinks.
pub fn expand_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(relative) => home.join(relative),
        Err(_) => path.to_owned(),
    }
}

/// Follow symlinks and require a regular file with at least one execute bit.
pub fn is_executable(path: &Path) -> io::Result<bool> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Search a supplied PATH in order, ignoring inaccessible candidates.
pub fn find_executable(program: &OsStr, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .map(|directory| directory.join(program))
        .find(|path| is_executable(path).unwrap_or(false))
}

/// Read UTF-8 text; only a missing file is treated as absent.
pub fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{ffi::OsStrExt, fs::symlink};

    #[test]
    fn home_expansion_only_replaces_a_leading_tilde_component() {
        let home = Path::new("/home/test");
        for (input, expected) in [
            ("~", "/home/test"),
            ("~/dir", "/home/test/dir"),
            ("~/../dir", "/home/test/../dir"),
            ("~someone/dir", "~someone/dir"),
            ("dir/~", "dir/~"),
            ("/dir", "/dir"),
            ("", ""),
        ] {
            assert_eq!(expand_home(Path::new(input), home), Path::new(expected));
        }
        let path = Path::new(OsStr::from_bytes(b"~/non-utf8-\xff"));
        assert_eq!(
            expand_home(path, home),
            home.join(OsStr::from_bytes(b"non-utf8-\xff"))
        );
    }

    #[test]
    fn executable_search_skips_invalid_candidates_and_preserves_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let program = second.join("tool");
        fs::write(&program, "executable").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o100)).unwrap();
        let search = std::env::join_paths([&first, &second]).unwrap();
        let candidate = first.join("tool");
        assert!(is_executable(&candidate).is_err());
        for directory in [false, true] {
            if directory {
                fs::remove_file(&candidate).unwrap();
                fs::create_dir(&candidate).unwrap();
            } else {
                fs::write(&candidate, "not executable").unwrap();
                fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(!is_executable(&candidate).unwrap());
            assert_eq!(
                find_executable(OsStr::new("tool"), &search),
                Some(program.clone())
            );
        }
        fs::remove_dir(&candidate).unwrap();
        symlink(&program, &candidate).unwrap();
        assert_eq!(
            find_executable(OsStr::new("tool"), &search),
            Some(candidate)
        );
        assert_eq!(find_executable(OsStr::new("missing"), &search), None);
    }

    #[test]
    fn optional_read_distinguishes_missing_empty_and_invalid_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        assert_eq!(read_optional(&path).unwrap(), None);
        fs::write(&path, "").unwrap();
        assert_eq!(read_optional(&path).unwrap(), Some(String::new()));
        fs::write(&path, "text").unwrap();
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("text"));
        fs::write(&path, [0xff]).unwrap();
        assert_eq!(
            read_optional(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(read_optional(root.path()).is_err());
        let link = root.path().join("link");
        symlink(root.path().join("missing"), &link).unwrap();
        assert_eq!(read_optional(&link).unwrap(), None);
    }
}
