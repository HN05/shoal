//! Small filesystem operations shared by the CLI and daemon.
use std::{
    ffi::OsStr,
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Copy the contents of a worktree without copying its Git administrative
/// file. The destination may already contain the clean checkout created for
/// the copied branch.
pub fn copy_worktree(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if entry.file_name() != ".git" && !source.join(entry.file_name()).symlink_metadata().is_ok()
        {
            remove_existing(&entry.path())?;
        }
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        remove_existing(destination)?;
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if file_type.is_dir() {
        if destination.is_symlink() || (destination.exists() && !destination.is_dir()) {
            remove_existing(destination)?;
        }
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        if destination.is_dir() && !destination.is_symlink() {
            fs::remove_dir_all(destination)?;
        }
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
    }
    Ok(())
}

fn remove_existing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

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

/// The absolute path this process was started through. Unlike `current_exe`
/// on Linux, it keeps an installation symlink instead of resolving into a
/// versioned directory, so a later run of it reaches an upgraded binary.
pub fn invoked_executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    Ok(invoked_path(
        std::env::args_os().next(),
        &std::env::var_os("PATH").unwrap_or_default(),
        &std::env::current_dir()?,
        current,
    ))
}

// Trust argv[0] only while it names the running binary.
fn invoked_path(
    arg: Option<std::ffi::OsString>,
    search_path: &OsStr,
    cwd: &Path,
    current: PathBuf,
) -> PathBuf {
    // A relative argv[0] or PATH entry names a file in the current directory.
    let invoked = arg.map(PathBuf::from).and_then(|arg| {
        if arg.components().count() > 1 {
            Some(cwd.join(arg))
        } else {
            std::env::split_paths(search_path)
                .map(|directory| cwd.join(directory).join(&arg))
                .find(|path| is_executable(path).unwrap_or(false))
        }
    });
    match invoked {
        Some(path) if same_file(&path, &current) => path,
        _ => current,
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => (left.dev(), left.ino()) == (right.dev(), right.ino()),
        _ => false,
    }
}

/// Read UTF-8 text; only a missing file is treated as absent.
pub fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Space unprivileged processes may still write on the filesystem holding `path`.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: the path is NUL terminated and statvfs fills the zeroed buffer.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so the buffer is initialized.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion)] // The field types differ by platform.
    Ok(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
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

    #[test]
    fn invoked_path_keeps_an_installation_symlink_to_the_running_binary() {
        let root = tempfile::tempdir().unwrap();
        let keg = root.path().join("keg");
        let bin = root.path().join("bin");
        fs::create_dir_all(&keg).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let binary = keg.join("shoal");
        let other = keg.join("other");
        for file in [&binary, &other] {
            fs::write(file, "").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let link = bin.join("shoal");
        std::os::unix::fs::symlink(&binary, &link).unwrap();
        let resolve = |arg: &str| {
            invoked_path(
                Some(arg.into()),
                bin.as_os_str(),
                root.path(),
                binary.clone(),
            )
        };
        assert_eq!(resolve(link.to_str().unwrap()), link);
        assert_eq!(resolve("shoal"), link, "found on PATH");
        assert_eq!(
            resolve("bin/shoal"),
            link,
            "relative to the current directory"
        );
        assert_eq!(
            invoked_path(
                Some("shoal".into()),
                "bin".as_ref(),
                root.path(),
                binary.clone()
            ),
            link,
            "found through a relative PATH entry"
        );
        assert_eq!(resolve(other.to_str().unwrap()), binary);
        assert_eq!(resolve("missing"), binary);
        assert_eq!(
            invoked_path(None, bin.as_os_str(), root.path(), binary.clone()),
            binary
        );
    }

    #[test]
    fn available_bytes_reads_the_filesystem_or_fails() {
        let root = tempfile::tempdir().unwrap();
        assert!(available_bytes(root.path()).unwrap() > 0);
        assert!(available_bytes(&root.path().join("missing")).is_err());
    }
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
