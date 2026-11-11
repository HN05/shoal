//! The working directories of this user's processes, read from the kernel.
use anyhow::Result;
use std::path::PathBuf;

/// A process and the directory it works in.
pub(super) struct WorkingDirectory {
    pub(super) pid: u32,
    /// Its command name, as the kernel records it.
    pub(super) name: String,
    pub(super) path: PathBuf,
}

/// The working directory of each of this user's live processes other than
/// this one; a process that exits meanwhile, or whose directory the kernel
/// does not tell, is left out.
pub(super) fn working_directories() -> Result<Vec<WorkingDirectory>> {
    let mut found = Vec::new();
    for pid in super::identity::same_user_pids()? {
        if let Some(directory) = working_directory(pid)? {
            found.push(directory);
        }
    }
    Ok(found)
}

#[cfg(target_os = "macos")]
fn working_directory(pid: u32) -> Result<Option<WorkingDirectory>> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    let Some(info) =
        super::identity::pid_info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO, 0)?
    else {
        return Ok(None);
    };
    let Some(paths) =
        super::identity::pid_info::<libc::proc_vnodepathinfo>(pid, libc::PROC_PIDVNODEPATHINFO, 0)?
    else {
        return Ok(None);
    };
    // Both are NUL-terminated C strings in fixed buffers.
    let name = unsafe { CStr::from_ptr(info.pbsi_comm.as_ptr()) };
    let path = unsafe { CStr::from_ptr(paths.pvi_cdir.vip_path.as_ptr().cast()) };
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(WorkingDirectory {
        pid,
        name: name.to_string_lossy().into_owned(),
        path: PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())),
    }))
}

#[cfg(target_os = "linux")]
fn working_directory(pid: u32) -> Result<Option<WorkingDirectory>> {
    let (Some(path), Some(name)) = (
        present(std::fs::read_link(format!("/proc/{pid}/cwd")))?,
        present(std::fs::read_to_string(format!("/proc/{pid}/comm")))?,
    ) else {
        return Ok(None);
    };
    Ok(Some(WorkingDirectory {
        pid,
        name: name.trim_end_matches('\n').to_owned(),
        path,
    }))
}

/// A `/proc` read, or `None` when the process exited, even partway through
/// the read, or hides the entry, as a kernel thread hides its working
/// directory.
#[cfg(target_os = "linux")]
fn present<T>(read: std::io::Result<T>) -> Result<Option<T>> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(error) if super::identity::process_record_unavailable(error.raw_os_error()) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
