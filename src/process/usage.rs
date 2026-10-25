//! Native per-process resource readings. A process that ended before it was
//! read has no reading.
use anyhow::{Context, Result, ensure};

/// One process's resource use when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub parent: u32,
    /// CPU time in nanoseconds, including the children it has waited for, so
    /// short-lived children count once they are reaped.
    pub cpu: u64,
    /// Bytes of memory it holds: its resident size on Linux and its physical
    /// footprint on macOS, so pages shared with others count in each.
    pub memory: u64,
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
    // User, system, and waited-for children's user and system time.
    let ticks = (11..=14).try_fold(0_u64, |sum, index| {
        sum.checked_add(number(index)?).context("CPU time overflow")
    })?;
    let cpu = u128::from(ticks) * 1_000_000_000 / u128::from(ticks_per_second);
    Ok(Reading {
        parent: fields[1].parse().context("invalid parent process")?,
        cpu: cpu.try_into().context("CPU time overflow")?,
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
    let ticks = [
        info.ri_user_time,
        info.ri_system_time,
        info.ri_child_user_time,
        info.ri_child_system_time,
    ]
    .into_iter()
    .try_fold(0_u64, |sum, value| sum.checked_add(value))
    .context("CPU time overflow")?;
    Ok(Some(Reading {
        parent,
        cpu: mach_nanoseconds(ticks)?,
        memory: info.ri_phys_footprint,
    }))
}

/// Usage times count Mach absolute time units, which are not nanoseconds on
/// Apple silicon.
#[cfg(target_os = "macos")]
fn mach_nanoseconds(ticks: u64) -> Result<u64> {
    #[allow(deprecated)]
    let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: the call fills the structure it is given.
    #[allow(deprecated)]
    let result = unsafe { libc::mach_timebase_info(&mut timebase) };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_readings_count_waited_children_and_resident_pages() {
        let stat = "42 (a (b) c) S 7 42 42 0 -1 4194560 1 2 3 4 \
                    100 50 20 30 20 0 1 0 999 1000 25 18446744073709551615";
        assert_eq!(
            linux_reading(stat, 100, 4096).unwrap(),
            Reading {
                parent: 7,
                cpu: 2_000_000_000,
                memory: 25 * 4096,
            }
        );
        assert!(linux_reading("42 (a) S 7 42", 100, 4096).is_err());
        assert!(linux_reading(&stat.replace(" 100 ", " x "), 100, 4096).is_err());
    }
}
