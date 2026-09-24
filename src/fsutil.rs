//! Small filesystem operations shared by the CLI and daemon.
use std::{
    ffi::OsStr,
    fs,
    io::{self, Write},
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

pub enum Permissions {
    /// Keep the private permissions chosen by NamedTempFile.
    Temporary,
    /// Copy the destination's permissions if its metadata can be read.
    Preserve,
    Mode(u32),
}

pub struct ReplaceOptions {
    pub permissions: Permissions,
    /// Sync the replacement file before publishing it; does not sync the directory.
    pub sync: bool,
}

/// Prepare beside the destination so a later persist stays on the same filesystem.
/// The caller creates parent directories and may move a backup before publishing.
pub fn prepare_atomic_write(
    path: &Path,
    contents: &[u8],
    options: ReplaceOptions,
) -> io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    let permissions = match options.permissions {
        Permissions::Temporary => None,
        Permissions::Preserve => fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions()),
        Permissions::Mode(mode) => Some(fs::Permissions::from_mode(mode)),
    };
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    if options.sync {
        temporary.as_file().sync_all()?;
    }
    Ok(temporary)
}

/// Replace the directory entry, including an existing symlink, with a complete file.
pub fn replace_atomically(path: &Path, contents: &[u8], options: ReplaceOptions) -> io::Result<()> {
    prepare_atomic_write(path, contents, options)?
        .persist(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{ffi::OsStrExt, fs::symlink};

    #[test]
    fn atomic_replace_applies_permissions_and_replaces_symlinks() {
        use std::io::Read;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let path = root.path().join("destination");
        for (permissions, expected, sync) in [
            (Permissions::Temporary, 0o600, false),
            (Permissions::Preserve, 0o640, true),
            (Permissions::Mode(0o644), 0o644, false),
        ] {
            fs::write(&source, "old").unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
            symlink(&source, &path).unwrap();
            let mut old = fs::File::open(&path).unwrap();
            replace_atomically(&path, b"new", ReplaceOptions { permissions, sync }).unwrap();
            assert!(!path.is_symlink());
            assert_eq!(fs::read_to_string(&path).unwrap(), "new");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                expected
            );
            let mut text = String::new();
            old.read_to_string(&mut text).unwrap();
            assert_eq!(text, "old");
            assert_eq!(fs::read_to_string(&source).unwrap(), "old");
            fs::remove_file(&path).unwrap();
        }
        replace_atomically(
            &path,
            b"created",
            ReplaceOptions {
                permissions: Permissions::Preserve,
                sync: false,
            },
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn failed_or_abandoned_replacements_leave_no_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("destination");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "old").unwrap();
        assert!(
            replace_atomically(
                &path,
                b"new",
                ReplaceOptions {
                    permissions: Permissions::Temporary,
                    sync: false,
                }
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "old");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        let prepared = prepare_atomic_write(
            &path,
            b"new",
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: false,
            },
        )
        .unwrap();
        assert!(path.is_dir());
        drop(prepared);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

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
