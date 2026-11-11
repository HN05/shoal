//! Btrfs subvolumes: directory trees whose writable snapshot copies only
//! their root, so it takes the same time for any number of files. Shoal
//! reaches them through the kernel's ioctls; on other filesystems and
//! systems no directory is a subvolume and making one fails.
use std::{
    fs, io,
    path::{Component, Path},
};

/// What Shoal does with subvolumes, so callers' choices between them and
/// plain copies can be tested with fakes.
pub trait Subvolumes {
    /// Whether `path` is a directory that is the root of a subvolume.
    fn is_root(&self, path: &Path) -> bool;
    /// Whether the subvolume `path` may hold others, whose files its
    /// snapshot would leave out; true when that cannot be read.
    fn holds_subvolumes(&self, path: &Path) -> bool;
    /// Make the missing `path` an empty subvolume.
    fn create(&self, path: &Path) -> io::Result<()>;
    /// Make the missing `destination` a writable snapshot of the subvolume
    /// `source`.
    fn snapshot(&self, source: &Path, destination: &Path) -> io::Result<()>;
    /// Delete the subvolume `path` with everything in it, which needs root
    /// or a mount with `user_subvol_rm_allowed`.
    fn destroy(&self, path: &Path) -> io::Result<()>;
}

/// The kernel's btrfs subvolumes.
pub struct Btrfs;

/// Delete the tree at `path`, if there is one. The subvolumes at `outputs`
/// below it, deepest first, and `path` itself go first, by destroying them
/// where the caller may; the recursive delete of what is left removes empty
/// subvolumes as directories, which needs Linux 4.18 or later.
pub fn remove_tree(volumes: &impl Subvolumes, path: &Path, outputs: &[String]) -> io::Result<()> {
    destroy_outputs(volumes, path, outputs);
    if volumes.is_root(path) {
        let _ = volumes.destroy(path);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Destroy the subvolumes at `outputs` of `root`, deepest first, so one
/// inside another goes before it. One that cannot be destroyed is left for
/// a recursive delete.
pub fn destroy_outputs(volumes: &impl Subvolumes, root: &Path, outputs: &[String]) {
    let mut outputs: Vec<&String> = outputs.iter().collect();
    outputs.sort_by_key(|output| std::cmp::Reverse(Path::new(output).components().count()));
    for output in outputs {
        if !lies_inside(root, output) {
            continue;
        }
        let path = root.join(output);
        if volumes.is_root(&path) {
            let _ = volumes.destroy(&path);
        }
    }
}

/// Whether Shoal may make subvolumes at `path`: it creates one there and
/// removes it again as [`remove_tree`] does, so it never keeps a subvolume
/// it cannot remove. A kernel without `rmdir` for subvolumes, or a mount
/// that refuses them, fails.
pub fn removable_at(volumes: &impl Subvolumes, path: &Path) -> bool {
    // A probe an earlier one left behind.
    if remove_tree(volumes, path, &[]).is_err() || volumes.create(path).is_err() {
        return false;
    }
    remove_tree(volumes, path, &[]).is_ok() && fs::symlink_metadata(path).is_err()
}

/// Whether `output` names an existing path inside `root`, reached through
/// no symbolic link, which could lead outside it.
fn lies_inside(root: &Path, output: &str) -> bool {
    let mut current = root.to_path_buf();
    for component in Path::new(output).components() {
        let Component::Normal(name) = component else {
            return false;
        };
        current.push(name);
        match current.symlink_metadata() {
            Ok(metadata) if !metadata.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    true
}

#[cfg(target_os = "linux")]
impl Subvolumes for Btrfs {
    fn is_root(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && metadata.ino() == ioctl::FIRST_FREE_OBJECTID)
            && ioctl::on_btrfs(path)
    }

    fn holds_subvolumes(&self, path: &Path) -> bool {
        let Ok(directory) = fs::File::open(path) else {
            return true;
        };
        // SAFETY: the arguments are plain integers, valid when zeroed.
        let mut args: ioctl::RootRefArgs = unsafe { std::mem::zeroed() };
        // Kernels before 4.18 lack the ioctl, so their answer is unknown.
        ioctl::call(&directory, ioctl::GET_SUBVOL_ROOTREF, &mut args).is_err() || args.num_items > 0
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        let (parent, name) = ioctl::parent_and_name(path)?;
        let mut args = ioctl::VolArgs {
            fd: 0,
            name: [0; 4088],
        };
        ioctl::set_name(&mut args.name, name)?;
        ioctl::call(&parent, ioctl::SUBVOL_CREATE, &mut args)
    }

    fn snapshot(&self, source: &Path, destination: &Path) -> io::Result<()> {
        use std::os::{fd::AsRawFd as _, unix::fs::OpenOptionsExt as _};
        let source = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(source)?;
        let (parent, name) = ioctl::parent_and_name(destination)?;
        let mut args = ioctl::VolArgsV2 {
            fd: i64::from(source.as_raw_fd()),
            transid: 0,
            flags: 0,
            unused: [0; 4],
            name: [0; 4040],
        };
        ioctl::set_name(&mut args.name, name)?;
        ioctl::call(&parent, ioctl::SNAP_CREATE_V2, &mut args)
    }

    fn destroy(&self, path: &Path) -> io::Result<()> {
        if !self.is_root(path) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let (parent, name) = ioctl::parent_and_name(path)?;
        let mut args = ioctl::VolArgs {
            fd: 0,
            name: [0; 4088],
        };
        ioctl::set_name(&mut args.name, name)?;
        ioctl::call(&parent, ioctl::SNAP_DESTROY, &mut args)
    }
}

#[cfg(not(target_os = "linux"))]
impl Subvolumes for Btrfs {
    fn is_root(&self, _: &Path) -> bool {
        false
    }

    fn holds_subvolumes(&self, _: &Path) -> bool {
        true
    }

    fn create(&self, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn snapshot(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn destroy(&self, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// The btrfs ioctls, as `linux/btrfs.h` defines them.
#[cfg(target_os = "linux")]
mod ioctl {
    use std::{
        ffi::OsStr,
        fs, io,
        os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
        path::Path,
    };

    /// The inode number of every subvolume's root directory.
    pub(super) const FIRST_FREE_OBJECTID: u64 = 256;
    const SUPER_MAGIC: u32 = 0x9123_683e;
    const MAGIC: u32 = 0x94;
    pub(super) const SUBVOL_CREATE: libc::Ioctl = libc::_IOW::<VolArgs>(MAGIC, 14);
    pub(super) const SNAP_DESTROY: libc::Ioctl = libc::_IOW::<VolArgs>(MAGIC, 15);
    pub(super) const SNAP_CREATE_V2: libc::Ioctl = libc::_IOW::<VolArgsV2>(MAGIC, 23);
    pub(super) const GET_SUBVOL_ROOTREF: libc::Ioctl = libc::_IOWR::<RootRefArgs>(MAGIC, 61);

    /// `struct btrfs_ioctl_vol_args`.
    #[repr(C)]
    pub(super) struct VolArgs {
        pub fd: i64,
        pub name: [u8; 4088],
    }

    /// `struct btrfs_ioctl_vol_args_v2`, without quota group inheritance.
    #[repr(C)]
    pub(super) struct VolArgsV2 {
        pub fd: i64,
        pub transid: u64,
        pub flags: u64,
        pub unused: [u64; 4],
        pub name: [u8; 4040],
    }

    /// `struct btrfs_ioctl_get_subvol_rootref_args`: the subvolumes whose
    /// parent is the one the descriptor is in.
    #[repr(C)]
    pub(super) struct RootRefArgs {
        pub min_treeid: u64,
        pub rootref: [[u64; 2]; 255],
        pub num_items: u8,
        pub align: [u8; 7],
    }

    /// Whether `path` lies on a btrfs filesystem.
    pub(super) fn on_btrfs(path: &Path) -> bool {
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `stat` is a valid, writable statfs for the call to fill.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let status = unsafe { libc::statfs(path.as_ptr(), &mut stat) };
        // The field's type differs between targets; the magic fits 32 bits.
        #[allow(clippy::unnecessary_cast)]
        let magic = stat.f_type as u32;
        status == 0 && magic == SUPER_MAGIC
    }

    /// The opened parent directory of `path`, on btrfs, and its last
    /// component.
    pub(super) fn parent_and_name(path: &Path) -> io::Result<(fs::File, &OsStr)> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if !on_btrfs(parent) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok((fs::File::open(parent)?, name))
    }

    /// Copy `name` into the NUL-terminated `field`.
    pub(super) fn set_name(field: &mut [u8], name: &OsStr) -> io::Result<()> {
        let bytes = name.as_bytes();
        if bytes.len() >= field.len() {
            return Err(io::ErrorKind::InvalidFilename.into());
        }
        field[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Run the ioctl `request` with `args` on the directory `directory`.
    pub(super) fn call<T>(
        directory: &fs::File,
        request: libc::Ioctl,
        args: &mut T,
    ) -> io::Result<()> {
        // SAFETY: `args` is the structure `request` reads and writes, alive
        // for the call.
        let status =
            unsafe { libc::ioctl(directory.as_raw_fd(), request, std::ptr::from_mut(args)) };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests;
