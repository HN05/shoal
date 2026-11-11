//! The macOS back end of process identity: processes, their lineage and
//! their environments as `proc_info` and `sysctl` show them.
use super::{Identity, Image, Lineage, Listed};
use crate::client::processes::UniqueId;
use anyhow::{Context, Result, ensure};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf};

/// macOS refuses the arguments of a process whose exec has not published them
/// yet, often right after reading them as empty.
pub(super) const REFUSED_WHILE_LOADING: bool = true;

/// Unique IDs tell the processes launchd adopted from those it started, so
/// launchd's children are not all detached.
pub(super) const INIT_CHILDREN_DETACHED: bool = false;

/// macOS refuses an exiting process's arguments, and its records soon after;
/// in between they flag the exit.
pub(super) fn image(pid: u32) -> Result<Image> {
    Ok(bsd_info(pid, false)?.map_or(Image::Ended, |info| bsd_image(&info.bsd)))
}

fn bsd_image(bsd: &libc::proc_bsdinfo) -> Image {
    // PROC_FLAG_INEXIT in sys/proc_info.h.
    const EXITING: u32 = 0x4;
    // BSD SZOMB = 5.
    if bsd.pbi_status == 5 || bsd.pbi_flags & EXITING != 0 {
        Image::Ended
    } else {
        Image::Settled
    }
}

/// macOS 27 hides the environments of its own platform binaries, such as
/// shells, from other processes; one that cannot be checked counts as hidden.
pub(super) fn hides_environment(pid: u32) -> bool {
    const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
    crate::process::code_signing::status_flags(pid as i32)
        .is_none_or(|flags| flags & CS_PLATFORM_BINARY != 0)
}

/// The process other than init that adopts this user's orphans.
pub(super) async fn subreaper() -> Result<Option<u32>> {
    // Launchd adopts every orphan, and unique IDs tell which it adopted.
    Ok(None)
}

/// The boot `identity` was recorded in, which its unique ID names.
pub(super) fn recorded_boot(identity: &Identity) -> Option<&str> {
    identity.unique.as_ref().map(|unique| unique.boot.as_str())
}

pub(super) fn current_boot() -> Option<&'static str> {
    boot_session().map(String::as_str)
}

pub(super) fn lineage(pid: u32) -> Result<Option<Lineage>> {
    let Some(info) = bsd_info(pid, false)? else {
        return Ok(None);
    };
    let bsd = &info.bsd;
    // BSD SZOMB = 5. Zombies cannot execute or hold resources.
    if bsd.pbi_uid != unsafe { libc::geteuid() } || bsd.pbi_status == 5 {
        return Ok(None);
    }
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth: format!("{}:{}", bsd.pbi_start_tvsec, bsd.pbi_start_tvusec),
            unique: boot_session().map(|boot| UniqueId {
                boot: boot.clone(),
                id: info.unique.unique,
            }),
        },
        unique: Some(info.unique.unique),
        parent: Some(info.unique.parent_unique),
        parent_pid: bsd.pbi_ppid,
        adopted: bsd.pbi_ppid == 1 && !started_by_launchd(&info.unique, launchd_version()),
    }))
}

/// The lineage of `pid`, a setuid program this user started, from the
/// records macOS shows for any process, since it refuses the full identity
/// of another user's process.
pub(super) fn elevated_lineage(pid: u32) -> Result<Option<Lineage>> {
    let read = || pid_info::<UniqueIdentifiers>(pid, PROC_PIDUNIQIDENTIFIERINFO, 0);
    let Some(unique) = read()? else {
        return Ok(None);
    };
    let (Some(short), Some(birth)) = (
        pid_info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO, 0)?,
        start_time(pid)?,
    ) else {
        return Ok(None);
    };
    // The process may have exited between the reads and left its PID to
    // another; unique IDs are never reused.
    let same = read()?.is_some_and(|again| again.unique == unique.unique);
    // BSD SZOMB = 5. Zombies cannot execute or hold resources.
    if !same || short.pbsi_status == 5 {
        return Ok(None);
    }
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth,
            unique: boot_session().map(|boot| UniqueId {
                boot: boot.clone(),
                id: unique.unique,
            }),
        },
        unique: Some(unique.unique),
        parent: Some(unique.parent_unique),
        parent_pid: short.pbsi_ppid,
        adopted: short.pbsi_ppid == 1 && !started_by_launchd(&unique, launchd_version()),
    }))
}

/// When `pid` started, in the form its identity records. `p_starttime` opens
/// `kinfo_proc`, which libc does not bind and macOS shows for any process.
fn start_time(pid: u32) -> Result<Option<String>> {
    // `sizeof(struct kinfo_proc)` on 64-bit macOS.
    const KINFO_PROC_SIZE: usize = 648;
    let mut record = [0_u8; KINFO_PROC_SIZE];
    let mut len = record.len();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as i32,
    ];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            record.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect process start");
    }
    // An exited process has no record.
    if len == 0 {
        return Ok(None);
    }
    ensure!(len == KINFO_PROC_SIZE, "incomplete process record");
    let seconds = i64::from_ne_bytes(record[..8].try_into()?);
    let micros = i32::from_ne_bytes(record[8..12].try_into()?);
    Ok(Some(format!("{seconds}:{micros}")))
}

/// This boot's session UUID, which names the boot unique process IDs belong to.
pub(super) fn boot_session() -> Option<&'static String> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        let mut bytes = [0_u8; 64];
        let mut len = bytes.len();
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return None;
        }
        let session = std::ffi::CStr::from_bytes_until_nul(bytes.get(..len)?).ok()?;
        Some(session.to_string_lossy().into_owned())
    })
    .as_ref()
}

/// Exec resets an adopted process's parent unique ID to launchd's, but not
/// the version of the PID it was forked from, so only launchd's version read
/// and matching shows that launchd started it.
fn started_by_launchd(process: &UniqueIdentifiers, launchd: Option<u32>) -> bool {
    // Launchd is the first process the kernel starts.
    const LAUNCHD_UNIQUE: u64 = 1;
    process.parent_unique == LAUNCHD_UNIQUE && launchd == Some(process.original_parent_version)
}

/// Launchd's PID version, which it keeps for the whole boot.
fn launchd_version() -> Option<u32> {
    static LAUNCHD_VERSION: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *LAUNCHD_VERSION.get_or_init(|| {
        let launchd = pid_info::<UniqueIdentifiers>(1, PROC_PIDUNIQIDENTIFIERINFO, 0);
        Some(launchd.ok()??.id_version as u32)
    })
}

pub(super) fn unreaped(identity: &Identity) -> Result<bool> {
    // SZOMB in sys/proc.h.
    const ZOMBIE: u32 = 5;
    let Some(info) = bsd_info(identity.pid, true)? else {
        return Ok(false);
    };
    let bsd = &info.bsd;
    Ok(bsd.pbi_status == ZOMBIE
        && bsd.pbi_uid == unsafe { libc::geteuid() }
        && format!("{}:{}", bsd.pbi_start_tvsec, bsd.pbi_start_tvusec) == identity.birth)
}

/// `pid`'s working directory, which macOS shows for a platform binary too.
pub(super) fn working_directory(pid: u32) -> Option<PathBuf> {
    let info = pid_info::<libc::proc_vnodepathinfo>(pid, libc::PROC_PIDVNODEPATHINFO, 0).ok()??;
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .as_flattened()
        .iter()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(&path)))
}

/// The file `pid` runs, which macOS shows for a platform binary too.
pub(crate) fn executable(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    let len = usize::try_from(len).ok().filter(|len| *len > 0)?;
    path.truncate(len);
    Some(PathBuf::from(std::ffi::OsString::from_vec(path)))
}

pub(in crate::process) fn parent(pid: u32) -> Option<u32> {
    bsd_info(pid, false)
        .ok()
        .flatten()
        .map(|info| info.bsd.pbi_ppid)
}

/// `proc_bsdinfowithuniqid` and `proc_uniqidentifierinfo` from XNU's private
/// `proc_info` flavors, which libc does not bind: one read gives the birth and
/// unique IDs together.
#[repr(C)]
struct BsdInfo {
    bsd: libc::proc_bsdinfo,
    unique: UniqueIdentifiers,
}

#[repr(C)]
#[allow(dead_code, reason = "unread fields fix the kernel's layout")]
struct UniqueIdentifiers {
    uuid: [u8; 16],
    unique: u64,
    parent_unique: u64,
    id_version: i32,
    original_parent_version: u32,
    reserved: [u64; 2],
}

const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
const PROC_PIDT_BSDINFOWITHUNIQID: i32 = 18;

fn bsd_info(pid: u32, include_zombies: bool) -> Result<Option<BsdInfo>> {
    // XNU only searches its zombie list when this argument is nonzero.
    pid_info(pid, PROC_PIDT_BSDINFOWITHUNIQID, u64::from(include_zombies))
}

/// `pid`'s `proc_pidinfo` record of `flavor`, a plain C struct, or `None`
/// when it has exited or is another user's.
pub(in crate::process) fn pid_info<T>(pid: u32, flavor: i32, argument: u64) -> Result<Option<T>> {
    let mut info = std::mem::MaybeUninit::<T>::zeroed();
    let size = std::mem::size_of::<T>();
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            flavor,
            argument,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        if process_identity_unavailable(error.raw_os_error()) {
            return Ok(None);
        }
        return Err(error).context("inspect process identity");
    }
    ensure!(result as usize == size, "incomplete process identity");
    Ok(Some(unsafe { info.assume_init() }))
}

pub(super) fn listed() -> Result<Vec<Listed>> {
    // PROC_ALL_PIDS in sys/proc_info.h.
    const PROC_ALL_PIDS: u32 = 1;
    let mut listed = Vec::new();
    for pid in listed_pids(PROC_ALL_PIDS, 0)? {
        let Some(info) = pid_info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO, 0)?
        else {
            continue;
        };
        // BSD SZOMB = 5. Zombies cannot execute or hold resources.
        if info.pbsi_status != 5 {
            listed.push(Listed {
                pid,
                uid: info.pbsi_uid,
                real_uid: info.pbsi_ruid,
                group: info.pbsi_pgid,
                parent: info.pbsi_ppid,
            });
        }
    }
    Ok(listed)
}

fn process_identity_unavailable(errno: Option<i32>) -> bool {
    // EPERM means proc_pidinfo could not inspect another user's process. It
    // cannot be an execution Shoal started, so treat it as a different PID.
    matches!(errno, Some(libc::ESRCH | libc::ENOENT | libc::EPERM))
}

pub(super) fn environment(pid: u32) -> Result<Vec<Vec<u8>>> {
    let capacity = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    ensure!(
        (1..=16 * 1024 * 1024).contains(&capacity),
        "invalid process argument limit"
    );
    let mut bytes = vec![0_u8; capacity as usize];
    let mut len = bytes.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(result == 0, "cannot inspect process environment");
    bytes.truncate(len);
    ensure!(bytes.len() >= 4, "incomplete process arguments");
    let argc = i32::from_ne_bytes(bytes[..4].try_into()?);
    ensure!(argc > 0, "process arguments unavailable");
    let mut offset = 4;
    while offset < bytes.len() && bytes[offset] != 0 {
        offset += 1;
    }
    while offset < bytes.len() && bytes[offset] == 0 {
        offset += 1;
    }
    for _ in 0..argc {
        while offset < bytes.len() && bytes[offset] != 0 {
            offset += 1;
        }
        offset += 1;
    }
    ensure!(offset <= bytes.len(), "incomplete process arguments");
    while offset < bytes.len() && bytes[offset] == 0 {
        offset += 1;
    }
    Ok(bytes[offset..]
        .split(|b| *b == 0)
        .take_while(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// The PIDs in process group `group`, including zombies.
pub(super) fn group_members(group: u32) -> Result<Vec<u32>> {
    // PROC_PGRP_ONLY in sys/proc_info.h.
    const PROC_PGRP_ONLY: u32 = 2;
    listed_pids(PROC_PGRP_ONLY, group)
}

/// The PIDs other than init's that `proc_listpids` lists for `kind` and
/// `argument`, including zombies.
fn listed_pids(kind: u32, argument: u32) -> Result<Vec<u32>> {
    const PID_SIZE: usize = std::mem::size_of::<libc::pid_t>();
    // A null buffer asks for room for every process on the machine.
    let list = |buffer: *mut libc::pid_t, len: usize| {
        let bytes =
            unsafe { libc::proc_listpids(kind, argument, buffer.cast(), (len * PID_SIZE) as i32) };
        if bytes < 0 {
            return Err(std::io::Error::last_os_error()).context("list processes");
        }
        Ok(bytes as usize / PID_SIZE)
    };
    let mut pids = vec![0; list(std::ptr::null_mut(), 0)?.max(1)];
    // A full buffer may have cut off processes started since it was sized.
    loop {
        let count = list(pids.as_mut_ptr(), pids.len())?;
        if count < pids.len() {
            pids.truncate(count);
            break;
        }
        pids.resize(pids.len() * 2, 0);
    }
    Ok(pids
        .into_iter()
        .filter(|pid| *pid > 1)
        .map(|pid| pid as u32)
        .collect())
}

/// Whether `own`, a birth's seconds and microseconds, is not before `other`.
/// One that does not parse is not.
pub(super) fn born_not_before(
    (own_prefix, own_end): (&str, &str),
    (other_prefix, other_end): (&str, &str),
) -> bool {
    match (
        own_prefix.parse::<u64>(),
        own_end.parse::<u64>(),
        other_prefix.parse::<u64>(),
        other_end.parse::<u64>(),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b) >= (c, d),
        _ => true,
    }
}

/// `identity` as recorded in `boot`, which its unique ID names.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn recorded_in(identity: Identity, boot: &str) -> Identity {
    let id = identity.unique.as_ref().map_or(1, |unique| unique.id);
    Identity {
        unique: Some(UniqueId {
            boot: boot.into(),
            id,
        }),
        ..identity
    }
}

#[cfg(test)]
mod tests;
