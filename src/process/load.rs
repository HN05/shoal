//! Native machine load readings. Unavailable or malformed readings are errors,
//! never evidence that an agent should be stopped.
use anyhow::{Context, Result, ensure};

#[cfg(target_os = "linux")]
pub fn critical_memory(used_percent: u8) -> Result<bool> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read memory availability")?;
    linux_memory(&text, used_percent)
}

#[cfg(any(target_os = "linux", test))]
fn linux_memory(text: &str, threshold: u8) -> Result<bool> {
    let read = |name: &str| -> Result<u64> {
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .with_context(|| format!("missing {name} in memory reading"))?;
        let mut fields = line.split_whitespace();
        let value = fields.next().context("missing memory value")?.parse()?;
        ensure!(
            fields.next() == Some("kB") && fields.next().is_none(),
            "invalid memory units"
        );
        Ok(value)
    };
    let total = read("MemTotal:")?;
    let available = read("MemAvailable:")?;
    ensure!(
        total > 0 && available <= total,
        "invalid memory availability"
    );
    Ok(u128::from(available) * 100 <= u128::from(total) * u128::from(100 - threshold))
}

#[cfg(target_os = "macos")]
pub fn critical_memory(_used_percent: u8) -> Result<bool> {
    Ok(memory_level()? == 4)
}

#[cfg(target_os = "macos")]
fn memory_level() -> Result<libc::c_int> {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&level);
    // SAFETY: sysctl writes at most size bytes into the initialized integer;
    // the name is NUL terminated and no new value is supplied.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("read memory pressure");
    }
    ensure!(
        size == std::mem::size_of_val(&level),
        "incomplete memory pressure reading"
    );
    ensure!(
        matches!(level, 1 | 2 | 4),
        "unknown memory pressure level {level}"
    );
    Ok(level)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn critical_memory(_used_percent: u8) -> Result<bool> {
    anyhow::bail!("memory pressure monitoring is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_uses_available_including_reclaimable_cache() {
        let reading = "MemTotal: 10000 kB\nMemFree: 1 kB\nMemAvailable: 500 kB\n";
        assert!(linux_memory(reading, 95).unwrap());
        assert!(linux_memory(&reading.replace("500", "499"), 95).unwrap());
        assert!(!linux_memory(&reading.replace("500", "501"), 95).unwrap());
        for reading in [
            "MemTotal: 0 kB\nMemAvailable: 0 kB",
            "MemTotal: 10 kB\nMemAvailable: 11 kB",
            "MemTotal: 10 kB",
            "MemTotal: 10 MB\nMemAvailable: 0 kB",
            "MemTotal: x kB\nMemAvailable: 0 kB",
        ] {
            assert!(linux_memory(reading, 95).is_err(), "{reading}");
        }
    }
}

/// Cumulative CPU ticks across every core. I/O wait is idle; Linux guest ticks
/// are already included in user/nice and must not be counted twice.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuTicks {
    pub(crate) busy: u64,
    pub(crate) total: u64,
}

impl CpuTicks {
    pub fn used_percent_since(self, previous: Self) -> Option<f64> {
        let total = self.total.checked_sub(previous.total)?;
        let busy = self.busy.checked_sub(previous.busy)?;
        (total > 0 && busy <= total).then(|| busy as f64 * 100.0 / total as f64)
    }
}

#[cfg(target_os = "linux")]
pub fn cpu_ticks() -> Result<CpuTicks> {
    linux_cpu(&std::fs::read_to_string("/proc/stat").context("read CPU ticks")?)
}

#[cfg(any(target_os = "linux", test))]
fn linux_cpu(text: &str) -> Result<CpuTicks> {
    let mut fields = text
        .lines()
        .next()
        .context("missing CPU reading")?
        .split_whitespace();
    ensure!(
        fields.next() == Some("cpu"),
        "missing aggregate CPU reading"
    );
    let values = fields
        .take(8)
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(values.len() >= 4, "incomplete CPU reading");
    let total = values
        .iter()
        .try_fold(0u64, |sum, value| sum.checked_add(*value))
        .context("CPU ticks overflow")?;
    let idle = values[3]
        .checked_add(values.get(4).copied().unwrap_or_default())
        .context("idle ticks overflow")?;
    Ok(CpuTicks {
        busy: total - idle,
        total,
    })
}

#[cfg(target_os = "macos")]
pub fn cpu_ticks() -> Result<CpuTicks> {
    // Retain one host port for the process lifetime instead of acquiring a new
    // Mach send right on every sample.
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    // libc retains the native ABI; avoid a new dependency for this single call.
    #[allow(deprecated)]
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    let mut info = std::mem::MaybeUninit::<libc::host_cpu_load_info>::zeroed();
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: the output buffer and count match HOST_CPU_LOAD_INFO's ABI.
    let result = unsafe {
        libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    ensure!(
        result == libc::KERN_SUCCESS && count == libc::HOST_CPU_LOAD_INFO_COUNT,
        "read CPU ticks: Mach error {result}"
    );
    let ticks = unsafe { info.assume_init() }.cpu_ticks.map(u64::from);
    let total = ticks.iter().sum();
    Ok(CpuTicks {
        total,
        busy: total - ticks[libc::CPU_STATE_IDLE as usize],
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cpu_ticks() -> Result<CpuTicks> {
    anyhow::bail!("CPU monitoring is unsupported on this platform")
}

#[cfg(test)]
mod cpu_tests {
    use super::*;

    #[test]
    fn cpu_usage_measures_all_cores_without_guest_double_counting() {
        let first = linux_cpu("cpu 100 0 100 800 0 0 0 0 100 0\ncpu0 1 2 3 4").unwrap();
        let next = linux_cpu("cpu 180 0 110 805 5 0 0 0 180 0").unwrap();
        assert_eq!(next.used_percent_since(first), Some(90.0));
        assert_eq!(first.used_percent_since(first), None);
        assert_eq!(first.used_percent_since(next), None);
        for text in [
            "cpu0 1 2 3 4",
            "cpu 1 2 3",
            "cpu x 2 3 4",
            "cpu 18446744073709551615 1 0 0",
        ] {
            assert!(linux_cpu(text).is_err(), "{text}");
        }
    }
}

/// Recovery requires headroom, and native normal pressure on macOS.
pub fn safe_memory(used_percent: u8) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        let _ = used_percent;
        Ok(memory_level()? == 1)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(!critical_memory(used_percent)?)
    }
}
