//! The Linux back end of process identity: processes, their lineage and
//! their environments as `/proc` shows them.
use super::{Identity, Image, Lineage, Listed};
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, time::Duration};

/// Linux reads the environment of a process whose exec has not published it
/// yet as empty instead of refusing it.
pub(super) const REFUSED_WHILE_LOADING: bool = false;

/// Linux cannot tell init's own children from those it adopted, so every one
/// counts as detached.
pub(super) const INIT_CHILDREN_DETACHED: bool = true;

pub(super) fn hides_environment(_pid: u32) -> bool {
    false
}

/// The process other than init that adopts this user's orphans.
pub(super) async fn subreaper() -> Result<Option<u32>> {
    subreaper::current().await
}

pub(super) fn boot_session() -> Option<&'static String> {
    None
}

/// The boot `identity` was recorded in, which starts its birth.
pub(super) fn recorded_boot(identity: &Identity) -> Option<&str> {
    identity.birth.split_once(':').map(|(boot, _)| boot)
}

pub(super) fn current_boot() -> Option<&'static str> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(boot.trim().to_owned())
    })
    .as_deref()
}

pub(super) fn lineage(pid: u32) -> Result<Option<Lineage>> {
    proc_lineage(pid, |owner| owner == unsafe { libc::geteuid() })
}

/// The lineage of `pid`, a setuid program this user started, whose records
/// another user owns.
pub(super) fn elevated_lineage(pid: u32) -> Result<Option<Lineage>> {
    proc_lineage(pid, |owner| owner != unsafe { libc::geteuid() })
}

/// The lineage of `pid` when `owned` accepts the owner of its records.
fn proc_lineage(pid: u32, owned: impl Fn(u32) -> bool) -> Result<Option<Lineage>> {
    use std::os::unix::fs::MetadataExt;
    let path = format!("/proc/{pid}");
    let metadata = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !owned(metadata.uid()) {
        return Ok(None);
    }
    let Some(stat) = read_process_stat(&format!("{path}/stat"))? else {
        return Ok(None);
    };
    let fields = stat_fields(&stat)?;
    ensure!(fields.len() > 19, "incomplete process stat");
    if fields[0] == "Z" || fields[0] == "X" {
        return Ok(None);
    }
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    // Linux keeps no record of an exited parent, so lineage ends with it.
    Ok(Some(Lineage {
        identity: Identity {
            pid,
            birth: format!("{}:{}", boot.trim(), fields[19]),
            unique: None,
        },
        unique: None,
        parent: None,
        parent_pid: fields[1].parse()?,
        // Linux shows every environment, so nothing depends on how a process
        // came to init. An inventory marks the children of a subreaper.
        adopted: false,
    }))
}

pub(super) fn unreaped(identity: &Identity) -> Result<bool> {
    let Some(stat) = read_process_stat(&format!("/proc/{}/stat", identity.pid))? else {
        return Ok(false);
    };
    let fields = stat_fields(&stat)?;
    ensure!(fields.len() > 19, "incomplete process stat");
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(fields[0] == "Z" && format!("{}:{}", boot.trim(), fields[19]) == identity.birth)
}

pub(super) fn working_directory(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

pub(crate) fn executable(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

pub(in crate::process) fn parent(pid: u32) -> Option<u32> {
    let stat = read_process_stat(&format!("/proc/{pid}/stat")).ok()??;
    stat_fields(&stat).ok()?.get(1)?.parse().ok()
}

pub(super) fn image(pid: u32) -> Result<Image> {
    match read_process_stat(&format!("/proc/{pid}/stat"))? {
        Some(stat) => stat_image(&stat),
        None => Ok(Image::Ended),
    }
}

fn stat_image(stat: &str) -> Result<Image> {
    const PF_EXITING: u64 = 0x4;
    const PF_KTHREAD: u64 = 0x0020_0000;
    let fields = stat_fields(stat)?;
    // Field 9 is flags and field 51 env_end, which stays 0 until exec
    // publishes the new environment.
    ensure!(fields.len() > 48, "incomplete process stat");
    if fields[6].parse::<u64>()? & (PF_EXITING | PF_KTHREAD) != 0 {
        return Ok(Image::Ended);
    }
    Ok(if fields[48] == "0" {
        Image::Loading
    } else {
        Image::Settled
    })
}

/// Fields after the parenthesized command name, starting with the state.
pub(in crate::process) fn stat_fields(stat: &str) -> Result<Vec<&str>> {
    Ok(stat
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect())
}

/// A `/proc` record of a process, whose name may be any bytes; `None` once
/// the process has exited or when `/proc` hides it.
pub(in crate::process) fn read_process_stat(path: &str) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(stat) => Ok(Some(String::from_utf8_lossy(&stat).into_owned())),
        Err(error) if process_record_unavailable(error.raw_os_error()) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(in crate::process) fn process_record_unavailable(errno: Option<i32>) -> bool {
    // Linux returns ESRCH if the process exits between open and read. A
    // `hidepid` mount shows another user's PID but refuses its records, as
    // `ps` finds; this user's own are never refused.
    matches!(
        errno,
        Some(libc::ENOENT | libc::ESRCH | libc::EACCES | libc::EPERM)
    )
}

pub(super) fn listed() -> Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        let Some(stat) = read_process_stat(&format!("/proc/{pid}/stat"))? else {
            continue;
        };
        let fields = stat_fields(&stat)?;
        ensure!(fields.len() > 2, "incomplete process stat");
        // Zombies cannot execute or hold resources.
        if fields[0] == "Z" || fields[0] == "X" {
            continue;
        }
        let Some(status) = read_process_stat(&format!("/proc/{pid}/status"))? else {
            continue;
        };
        let users = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .context("process status names no user")?;
        let mut users = users.split_whitespace().map(str::parse::<u32>);
        let (Some(real_uid), Some(uid)) = (users.next(), users.next()) else {
            anyhow::bail!("incomplete process users");
        };
        listed.push(Listed {
            pid,
            uid: uid?,
            real_uid: real_uid?,
            group: fields[2].parse()?,
            parent: fields[1].parse()?,
        });
    }
    Ok(listed)
}

pub(super) fn environment(pid: u32) -> Result<Vec<Vec<u8>>> {
    Ok(std::fs::read(format!("/proc/{pid}/environ"))?
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// The arguments `pid` runs with, `None` once it has exited or for a kernel
/// thread, which has none.
pub(super) fn arguments(pid: u32) -> Option<Vec<Vec<u8>>> {
    let command_line = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(split_command_line(&command_line)).filter(|arguments| !arguments.is_empty())
}

/// The arguments of a `/proc/<pid>/cmdline` record, each terminated by a NUL.
fn split_command_line(bytes: &[u8]) -> Vec<Vec<u8>> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.split(|b| *b == 0).map(<[u8]>::to_vec).collect()
}

/// How long `pid` has run, `None` once it has exited.
pub(super) fn elapsed(pid: u32) -> Result<Option<Duration>> {
    let Some(fields) = live_stat(pid)? else {
        return Ok(None);
    };
    let started: u64 = fields[19].parse().context("invalid process start")?;
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the structure it is given.
    let read = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) };
    ensure!(read == 0, "read time since boot");
    let uptime = Duration::new(now.tv_sec as u64, now.tv_nsec as u32);
    Ok(Some(elapsed_since(
        started,
        sysconf(libc::_SC_CLK_TCK)?,
        uptime,
    )))
}

/// How long a process started `started` clock ticks after boot has run at
/// `uptime`.
fn elapsed_since(started: u64, ticks_per_second: u64, uptime: Duration) -> Duration {
    let nanoseconds = u128::from(started) * 1_000_000_000 / u128::from(ticks_per_second);
    uptime.saturating_sub(Duration::from_nanos(
        u64::try_from(nanoseconds).unwrap_or(u64::MAX),
    ))
}

/// The device of `pid`'s controlling terminal, `None` without one.
pub(super) fn terminal(pid: u32) -> Result<Option<PathBuf>> {
    let Some(fields) = live_stat(pid)? else {
        return Ok(None);
    };
    Ok(terminal_device(
        fields[4].parse().context("invalid process terminal")?,
    ))
}

/// The path in `/dev` of a pseudo-terminal or virtual console that `stat`'s
/// `tty_nr` encodes; other devices count as no terminal.
fn terminal_device(tty_nr: u32) -> Option<PathBuf> {
    // UNIX98_PTY_SLAVE_MAJOR and TTY_MAJOR in linux/major.h.
    const PTY_SLAVE_MAJORS: std::ops::RangeInclusive<u32> = 136..=143;
    const TTY_MAJOR: u32 = 4;
    // Virtual consoles take the tty major's first 64 minors, serial ports the rest.
    const CONSOLES: u32 = 64;
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    if PTY_SLAVE_MAJORS.contains(&major) {
        let number = (major - PTY_SLAVE_MAJORS.start()) * 256 + minor;
        Some(PathBuf::from(format!("/dev/pts/{number}")))
    } else if major == TTY_MAJOR && minor < CONSOLES {
        Some(PathBuf::from(format!("/dev/tty{minor}")))
    } else {
        None
    }
}

/// The foreground process group of `pid`'s controlling terminal, `None`
/// without one.
pub(super) fn foreground_group(pid: u32) -> Result<Option<u32>> {
    let Some(fields) = live_stat(pid)? else {
        return Ok(None);
    };
    let group: i32 = fields[5]
        .parse()
        .context("invalid foreground process group")?;
    Ok(u32::try_from(group).ok().filter(|group| *group > 0))
}

/// The `stat` fields of `pid`, from its state on, `None` once it has exited.
fn live_stat(pid: u32) -> Result<Option<Vec<String>>> {
    let Some(stat) = read_process_stat(&format!("/proc/{pid}/stat"))? else {
        return Ok(None);
    };
    let fields = stat_fields(&stat)?;
    ensure!(fields.len() > 19, "incomplete process stat");
    if fields[0] == "Z" || fields[0] == "X" {
        return Ok(None);
    }
    Ok(Some(fields.into_iter().map(str::to_owned).collect()))
}

/// The system configuration value `name`.
pub(in crate::process) fn sysconf(name: libc::c_int) -> Result<u64> {
    // SAFETY: sysconf reads a configuration value and has no other effects.
    let value = unsafe { libc::sysconf(name) };
    ensure!(value > 0, "invalid system configuration value");
    Ok(value as u64)
}

/// The PIDs in process group `group`, including zombies. Linux lists
/// process groups only per process; `getpgid` reads each one without
/// parsing another process's `/proc` record, which may not parse.
pub(super) fn group_members(group: u32) -> Result<Vec<u32>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        // Reading a process group sends no signal. A process that exited
        // since the listing has none.
        let member = unsafe { libc::getpgid(pid) };
        if pid > 1 && u32::try_from(member) == Ok(group) {
            members.push(pid as u32);
        }
    }
    Ok(members)
}

/// Whether `own`, a birth's boot and start time, is not before `other`: one
/// of another boot is before it, and one whose start time does not parse is
/// not.
pub(super) fn born_not_before(
    (own_prefix, own_end): (&str, &str),
    (other_prefix, other_end): (&str, &str),
) -> bool {
    own_prefix == other_prefix
        && own_end
            .parse::<u64>()
            .ok()
            .zip(other_end.parse::<u64>().ok())
            .is_none_or(|(a, b)| a >= b)
}

/// `identity` as recorded in `boot`, which starts its birth.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn recorded_in(identity: Identity, boot: &str) -> Identity {
    let start = identity
        .birth
        .split_once(':')
        .map_or("", |(_, start)| start);
    Identity {
        birth: format!("{boot}:{start}"),
        ..identity
    }
}

mod subreaper;
#[cfg(test)]
mod tests;
