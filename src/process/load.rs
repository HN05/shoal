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
    let memory = linux_memory_reading(text)?;
    Ok(
        u128::from(memory.available) * 100
            <= u128::from(memory.total) * u128::from(100 - threshold),
    )
}

/// The total and available memory `/proc/meminfo`'s `text` reports.
#[cfg(any(target_os = "linux", test))]
fn linux_memory_reading(text: &str) -> Result<Memory> {
    let read = |name: &str| -> Result<u64> {
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .with_context(|| format!("missing {name} in memory reading"))?;
        let mut fields = line.split_whitespace();
        let value: u64 = fields.next().context("missing memory value")?.parse()?;
        ensure!(
            fields.next() == Some("kB") && fields.next().is_none(),
            "invalid memory units"
        );
        Ok(value * 1024)
    };
    let total = read("MemTotal:")?;
    let available = read("MemAvailable:")?;
    ensure!(
        total > 0 && available <= total,
        "invalid memory availability"
    );
    Ok(Memory { total, available })
}

/// The machine's memory in bytes: all of it, and what new work can use
/// without pushing other memory out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
}

#[cfg(target_os = "linux")]
pub fn memory() -> Result<Memory> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read memory availability")?;
    linux_memory_reading(&text)
}

/// macOS reports the percentage of memory available beside its pressure level.
#[cfg(target_os = "macos")]
pub fn memory() -> Result<Memory> {
    let total: u64 = sysctl(c"hw.memsize")?;
    let level: libc::c_int = sysctl(c"kern.memorystatus_level")?;
    ensure!(
        total > 0 && (0..=100).contains(&level),
        "invalid memory availability"
    );
    Ok(Memory {
        total,
        available: (u128::from(total) * level as u128 / 100) as u64,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn memory() -> Result<Memory> {
    anyhow::bail!("memory monitoring is unsupported on this platform")
}

#[cfg(target_os = "macos")]
pub fn critical_memory(_used_percent: u8) -> Result<bool> {
    Ok(memory_level()? == 4)
}

#[cfg(target_os = "macos")]
fn memory_level() -> Result<libc::c_int> {
    let level: libc::c_int =
        sysctl(c"kern.memorystatus_vm_pressure_level").context("read memory pressure")?;
    ensure!(
        matches!(level, 1 | 2 | 4),
        "unknown memory pressure level {level}"
    );
    Ok(level)
}

/// The integer the sysctl `name` holds.
#[cfg(target_os = "macos")]
fn sysctl<T: Default>(name: &std::ffi::CStr) -> Result<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: sysctl writes at most size bytes into the initialized value;
    // the name is NUL terminated and no new value is supplied.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut T).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("read {}", name.to_string_lossy()));
    }
    ensure!(
        size == std::mem::size_of::<T>(),
        "incomplete {} reading",
        name.to_string_lossy()
    );
    Ok(value)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn critical_memory(_used_percent: u8) -> Result<bool> {
    anyhow::bail!("memory pressure monitoring is unsupported on this platform")
}

#[cfg(test)]
mod tests;

/// Cumulative CPU ticks across every core. I/O wait and niced time are idle:
/// work at low priority, such as CI jobs, takes only cores nothing else
/// wants. Linux guest ticks are already included in user/nice and must not be
/// counted twice.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
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
    // nice, idle and iowait
    let idle = [1, 3, 4]
        .into_iter()
        .try_fold(0u64, |sum, index| {
            sum.checked_add(values.get(index).copied().unwrap_or_default())
        })
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
    Ok(mach_cpu(
        unsafe { info.assume_init() }.cpu_ticks.map(u64::from),
    ))
}

/// Mach's CPU states, as `host_cpu_load_info` orders its ticks.
#[cfg(any(target_os = "macos", test))]
const MACH_IDLE: usize = 2;
#[cfg(any(target_os = "macos", test))]
const MACH_NICE: usize = 3;
#[cfg(target_os = "macos")]
const _: () = assert!(
    MACH_IDLE == libc::CPU_STATE_IDLE as usize && MACH_NICE == libc::CPU_STATE_NICE as usize
);

/// The busy and total ticks of Mach's per-state `ticks`.
#[cfg(any(target_os = "macos", test))]
fn mach_cpu(ticks: [u64; 4]) -> CpuTicks {
    let total = ticks.iter().sum();
    CpuTicks {
        total,
        busy: total - ticks[MACH_IDLE] - ticks[MACH_NICE],
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cpu_ticks() -> Result<CpuTicks> {
    anyhow::bail!("CPU monitoring is unsupported on this platform")
}

#[cfg(test)]
mod cpu_tests;

/// Warnings start where recovery headroom ends: native warning pressure on macOS.
pub fn memory_warning(used_percent: u8) -> Result<bool> {
    Ok(!safe_memory(used_percent)?)
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
