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
mod tests;
