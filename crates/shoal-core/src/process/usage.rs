//! Native per-process resource readings. A process that ended before it was
//! read has no reading.
use anyhow::{Context, Result, ensure};

/// One process's resource use when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub parent: u32,
    /// CPU time it used itself, in nanoseconds.
    pub cpu: u64,
    /// CPU time of the children it has waited for and theirs, in
    /// nanoseconds, so short-lived children count once they are reaped.
    pub reaped: u64,
    /// Bytes of memory it holds: its resident size on Linux and its physical
    /// footprint on macOS, so pages shared with others count in each.
    pub memory: u64,
}

impl Reading {
    /// All its CPU time, its own and its reaped children's.
    pub fn total(&self) -> u64 {
        self.cpu.saturating_add(self.reaped)
    }
}

#[cfg(target_os = "linux")]
pub fn read(pid: u32) -> Result<Option<Reading>> {
    let Some(stat) = super::identity::read_process_stat(&format!("/proc/{pid}/stat"))? else {
        return Ok(None);
    };
    linux_reading(
        &stat,
        sysconf(libc::_SC_CLK_TCK)?,
        sysconf(libc::_SC_PAGESIZE)?,
    )
    .map(Some)
}

#[cfg(target_os = "linux")]
fn sysconf(name: libc::c_int) -> Result<u64> {
    // SAFETY: sysconf reads a configuration value and has no other effects.
    let value = unsafe { libc::sysconf(name) };
    ensure!(value > 0, "invalid system configuration value");
    Ok(value as u64)
}

#[cfg(target_os = "linux")]
fn linux_reading(stat: &str, ticks_per_second: u64, page_size: u64) -> Result<Reading> {
    let fields = super::identity::stat_fields(stat)?;
    ensure!(fields.len() > 21, "incomplete process stat");
    let number = |index: usize| -> Result<u64> {
        fields[index].parse().context("invalid process usage field")
    };
    // User and system time, then waited-for children's.
    let nanoseconds = |user: usize| -> Result<u64> {
        let ticks = number(user)?
            .checked_add(number(user + 1)?)
            .context("CPU time overflow")?;
        (u128::from(ticks) * 1_000_000_000 / u128::from(ticks_per_second))
            .try_into()
            .context("CPU time overflow")
    };
    Ok(Reading {
        parent: fields[1].parse().context("invalid parent process")?,
        cpu: nanoseconds(11)?,
        reaped: nanoseconds(13)?,
        memory: number(21)?
            .checked_mul(page_size)
            .context("memory size overflow")?,
    })
}

#[cfg(target_os = "macos")]
pub fn read(pid: u32) -> Result<Option<Reading>> {
    let Some(parent) = super::identity::parent(pid) else {
        return Ok(None);
    };
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: the buffer is large enough for the RUSAGE_INFO_V2 flavor.
    let result = unsafe {
        libc::proc_pid_rusage(pid as i32, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
            return Ok(None);
        }
        return Err(error).context("read process usage");
    }
    // SAFETY: proc_pid_rusage filled the structure.
    let info = unsafe { info.assume_init() };
    let nanoseconds = |user: u64, system: u64| {
        mach_nanoseconds(user.checked_add(system).context("CPU time overflow")?)
    };
    Ok(Some(Reading {
        parent,
        cpu: nanoseconds(info.ri_user_time, info.ri_system_time)?,
        reaped: nanoseconds(info.ri_child_user_time, info.ri_child_system_time)?,
        memory: info.ri_phys_footprint,
    }))
}

/// Usage times count Mach absolute time units, which are not nanoseconds on
/// Apple silicon.
#[cfg(target_os = "macos")]
fn mach_nanoseconds(ticks: u64) -> Result<u64> {
    // libc deprecates its Mach declarations in favor of the mach2 crate.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    let mut timebase = Timebase { numer: 0, denom: 0 };
    // SAFETY: the call fills the structure it is given.
    let result = unsafe { mach_timebase_info(&mut timebase) };
    ensure!(
        result == libc::KERN_SUCCESS && timebase.denom > 0,
        "read CPU timebase: Mach error {result}"
    );
    (u128::from(ticks) * u128::from(timebase.numer) / u128::from(timebase.denom))
        .try_into()
        .context("CPU time overflow")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn read(_pid: u32) -> Result<Option<Reading>> {
    anyhow::bail!("process usage is unsupported on this platform")
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
