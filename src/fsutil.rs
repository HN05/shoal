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
/// the copied branch; entries the source lacks are removed at every depth, so
/// deletions in the source stay deleted.
pub fn copy_worktree(source: &Path, destination: &Path) -> io::Result<()> {
    mirror_children(source, destination, |name| name != ".git")
}

/// Make `destination`'s children match `source`'s, considering only names
/// `include` accepts on either side.
fn mirror_children(
    source: &Path,
    destination: &Path,
    include: fn(&OsStr) -> bool,
) -> io::Result<()> {
    remove_entries_missing_from(source, destination, include)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if include(&entry.file_name()) {
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn remove_entries_missing_from(
    source: &Path,
    destination: &Path,
    include: fn(&OsStr) -> bool,
) -> io::Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if include(&entry.file_name()) && source.join(entry.file_name()).symlink_metadata().is_err()
        {
            remove_existing(&entry.path())?;
        }
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
        mirror_children(source, destination, |_| true)?;
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        // Copying onto an existing path would write through a symlink or a
        // hard link to a file outside the destination.
        remove_existing(destination)?;
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

/// What tells a file at a path apart from its replacement. It follows
/// symlinks, so retargeting a link, renaming another file over it or rewriting
/// it in place all change the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: std::time::SystemTime,
}

impl FileIdentity {
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    /// Whether another file now sits at `path`. A missing or unreadable file
    /// is no replacement.
    pub fn replaced_at(&self, path: &Path) -> bool {
        Self::of(path).is_ok_and(|current| current != *self)
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

/// The file naming the owner of a directory Shoal created beside workspaces.
pub const OWNER_MARKER: &str = "owner";

/// Serializes creating marked directories, so no caller in this process finds
/// one another has created but not yet marked. Only the daemon creates them.
static CREATING_MARKED: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Create `root` marked as `owner`'s when it is missing; an existing one,
/// even one appearing meanwhile, is never claimed and is left for
/// [`is_marked_dir`] to judge.
pub fn create_marked_dir(root: &Path, owner: &str) -> io::Result<()> {
    let _creating = CREATING_MARKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(root) {
        Ok(()) => fs::write(root.join(OWNER_MARKER), format!("{owner}\n")),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether `root` is a real directory marked as `owner`'s, so nothing Shoal
/// creates or deletes there is reached through a symlink or belongs to anyone
/// else.
pub fn is_marked_dir(root: &Path, owner: &str) -> bool {
    fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir())
        && fs::read_to_string(root.join(OWNER_MARKER))
            .is_ok_and(|marked| marked.trim_end() == owner)
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

/// Clone `source`, a file or directory tree, to the missing `destination`
/// without copying data: APFS clones on macOS and reflinks on Linux, keeping
/// modification times. Fails instead of falling back to a full copy, which
/// would cost the disk space cloning saves.
pub async fn clone_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;
        // clone(2) flag: clone a symlink itself instead of its target.
        const CLONE_NOFOLLOW: u32 = 0x0001;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        tokio::task::spawn_blocking(move || {
            // SAFETY: both arguments are NUL-terminated paths that outlive the call.
            let status =
                unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), CLONE_NOFOLLOW) };
            if status == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
        .await?
        .with_context(|| format!("clone {}", destination.display()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut command = tokio::process::Command::new("cp");
        command
            .args([
                "--reflink=always",
                "--archive",
                "--no-target-directory",
                "--",
            ])
            .arg(source)
            .arg(destination);
        let result = crate::subprocess::output(command).await;
        if result.is_err() {
            let _ = remove_existing(destination);
        }
        result
            .map(drop)
            .with_context(|| format!("clone {}", destination.display()))
    }
}

/// How [`move_directory`] moved a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    Renamed,
    /// Copied to another filesystem; the source is still there.
    Copied,
}

/// Move the directory `source` onto the empty directory `destination`: a
/// rename on one filesystem, otherwise a copy that keeps symlinks, modes and
/// times and leaves `source` for the caller to delete once it no longer needs
/// it. A failed copy leaves `destination` partly filled.
pub async fn move_directory(source: &Path, destination: &Path) -> anyhow::Result<Moved> {
    match fs::rename(source, destination) {
        Ok(()) => return Ok(Moved::Renamed),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("move {} to {}", source.display(), destination.display())
            });
        }
    }
    copy_directory(source, destination).await?;
    Ok(Moved::Copied)
}

/// Copy the entries of `source` into `destination`, leaving the destination
/// directory's own attributes, and with them its identity, unchanged.
pub(crate) async fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let entries = fs::read_dir(source)
        .and_then(|entries| {
            entries
                .map(|entry| Ok(entry?.path()))
                .collect::<io::Result<Vec<_>>>()
        })
        .with_context(|| format!("read {}", source.display()))?;
    if entries.is_empty() {
        return Ok(());
    }
    let mut command = tokio::process::Command::new("cp");
    command.args(["-pPR", "--"]).args(entries).arg(destination);
    crate::subprocess::output(command)
        .await
        .map(drop)
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copying_a_worktree_replaces_destination_links_instead_of_following_them() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let outside = temp.path().join("outside");
        fs::write(&outside, "unrelated")?;
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&destination)?;
        // The source replaced tracked links with regular files; the clean
        // checkout still has the links, one absolute and one hard.
        for name in ["symlink", "hardlink"] {
            fs::write(source.join(name), "worktree")?;
            fs::set_permissions(source.join(name), fs::Permissions::from_mode(0o755))?;
        }
        std::os::unix::fs::symlink(&outside, destination.join("symlink"))?;
        fs::hard_link(&outside, destination.join("hardlink"))?;

        copy_worktree(&source, &destination)?;

        assert_eq!(fs::read_to_string(&outside)?, "unrelated");
        assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o600);
        for name in ["symlink", "hardlink"] {
            let copied = destination.join(name);
            assert!(fs::symlink_metadata(&copied)?.file_type().is_file());
            assert_eq!(fs::read_to_string(&copied)?, "worktree");
            assert_eq!(fs::metadata(&copied)?.permissions().mode() & 0o777, 0o755);
        }
        Ok(())
    }

    #[tokio::test]
    async fn moving_a_directory_keeps_its_contents_and_links() -> anyhow::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(source.join("nested/file"), "contents")?;
        fs::set_permissions(
            source.join("nested/file"),
            fs::Permissions::from_mode(0o750),
        )?;
        std::os::unix::fs::symlink("nested/file", source.join("link"))?;

        let copied = temp.path().join("copied");
        fs::create_dir(&copied)?;
        let before = fs::metadata(&copied)?;
        copy_directory(&source, &copied).await?;
        // The destination itself keeps its identity.
        let after = fs::metadata(&copied)?;
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.created().ok(), before.created().ok());
        assert_eq!(fs::read_to_string(copied.join("nested/file"))?, "contents");
        assert_eq!(
            fs::metadata(copied.join("nested/file"))?
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::read_link(copied.join("link"))?,
            Path::new("nested/file")
        );
        assert!(source.join("nested/file").exists());

        let renamed = temp.path().join("renamed");
        fs::create_dir(&renamed)?;
        assert_eq!(move_directory(&source, &renamed).await?, Moved::Renamed);
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(renamed.join("nested/file"))?, "contents");
        Ok(())
    }

    #[test]
    fn copying_a_worktree_removes_entries_deleted_at_any_depth() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(source.join(".git"), "gitdir: source")?;
        fs::write(source.join("nested/kept"), "changed")?;

        // The clean checkout still has what the source deleted.
        let destination = temp.path().join("destination");
        fs::create_dir_all(destination.join("nested/gone/deeper"))?;
        fs::write(destination.join(".git"), "gitdir: destination")?;
        fs::write(destination.join("removed"), "old")?;
        fs::write(destination.join("nested/kept"), "old")?;
        fs::write(destination.join("nested/deleted"), "old")?;
        fs::write(destination.join("nested/gone/deeper/file"), "old")?;

        copy_worktree(&source, &destination)?;
        assert_eq!(
            fs::read_to_string(destination.join(".git"))?,
            "gitdir: destination"
        );
        assert_eq!(
            fs::read_to_string(destination.join("nested/kept"))?,
            "changed"
        );
        assert!(!destination.join("removed").exists());
        assert!(!destination.join("nested/deleted").exists());
        assert!(!destination.join("nested/gone").exists());
        Ok(())
    }

    #[test]
    fn marked_directories_are_never_found_unmarked_or_claimed() {
        let temp = tempfile::tempdir().unwrap();
        for attempt in 0..20 {
            let root = temp.path().join(format!("repo-{attempt}/.caches"));
            let start = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    scope.spawn(|| {
                        start.wait();
                        create_marked_dir(&root, "repo").unwrap();
                        assert!(is_marked_dir(&root, "repo"));
                    });
                }
            });
        }
        // A directory someone else created is never claimed.
        let theirs = temp.path().join("theirs");
        fs::create_dir(&theirs).unwrap();
        create_marked_dir(&theirs, "repo").unwrap();
        assert!(!is_marked_dir(&theirs, "repo"));
        assert_eq!(fs::read_dir(&theirs).unwrap().count(), 0);
    }

    #[test]
    fn file_identity_changes_with_a_new_file_or_link_target() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let link = temp.path().join("shoal");
        fs::write(&first, "old").unwrap();
        fs::write(&second, "new binary").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let original = FileIdentity::of(&link).unwrap();
        assert!(!original.replaced_at(&link));

        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(original.replaced_at(&link));

        let original = FileIdentity::of(&first).unwrap();
        fs::rename(&second, &first).unwrap();
        assert!(original.replaced_at(&first));
        fs::remove_file(&first).unwrap();
        assert!(
            !original.replaced_at(&first),
            "a missing file is no replacement"
        );
    }

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
