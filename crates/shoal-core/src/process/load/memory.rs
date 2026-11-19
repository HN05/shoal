use anyhow::{Context, Result, ensure};

/// The machine's memory in bytes: all of it, and what new work can use
/// without pushing other memory out. On Linux both count swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
}

impl Memory {
    /// Whether at least `percent` of the memory is in use.
    #[cfg(any(target_os = "linux", test))]
    fn used_at_least(self, percent: u8) -> bool {
        u128::from(self.available) * 100
            <= u128::from(self.total) * u128::from(100u8.saturating_sub(percent))
    }
}

/// Where overload protection acts on memory, in percent: the share of memory
/// in use, and on Linux the share of time tasks stall waiting for memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLevel {
    pub used_percent: u8,
    pub stall_percent: u8,
}

/// The memory levels at which overload protection stops work, warns agents
/// and lets stopped agents recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLevels {
    pub stop: MemoryLevel,
    pub warning: MemoryLevel,
    pub recovery: MemoryLevel,
}

/// How one memory reading stands against [`MemoryLevels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPressure {
    /// Memory in use is past its stop level, or macOS reports critical
    /// pressure: work stops at once.
    pub critical: bool,
    /// Every task stalls on memory past its stop level, which stops work
    /// once it has held.
    pub stalled: bool,
    /// Past a warning level.
    pub warning: bool,
    /// Below every recovery level.
    pub safe: bool,
}

/// One reading of the machine's memory and its pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReading {
    pub memory: Memory,
    pub pressure: MemoryPressure,
}

/// Read memory in use with swap, and the stall where the kernel reports it.
#[cfg(target_os = "linux")]
pub fn memory(levels: &MemoryLevels) -> Result<MemoryReading> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read memory availability")?;
    let memory = linux_memory(&text)?;
    Ok(MemoryReading {
        memory,
        pressure: linux_pressure(memory, read_stall()?, levels),
    })
}

/// The memory stall the kernel reports, `None` without PSI: a kernel built
/// without it, or booted with `psi=0`, has no pressure file.
#[cfg(target_os = "linux")]
fn read_stall() -> Result<Option<Stall>> {
    match std::fs::read_to_string("/proc/pressure/memory") {
        Ok(text) => linux_stall(&text).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read memory stall"),
    }
}

/// The total and available memory, RAM and swap together, that
/// `/proc/meminfo`'s `text` reports.
#[cfg(any(target_os = "linux", test))]
fn linux_memory(text: &str) -> Result<Memory> {
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
        value.checked_mul(1024).context("memory reading overflow")
    };
    let total = read("MemTotal:")?;
    let available = read("MemAvailable:")?;
    let swap_total = read("SwapTotal:")?;
    let swap_free = read("SwapFree:")?;
    ensure!(
        total > 0 && available <= total && swap_free <= swap_total,
        "invalid memory availability"
    );
    Ok(Memory {
        total: total
            .checked_add(swap_total)
            .context("memory reading overflow")?,
        available: available + swap_free,
    })
}

/// The share of time, in percent, that tasks stalled waiting for memory:
/// some of them over ten seconds and over a minute, and all of them over ten
/// seconds.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stall {
    some_avg10: f64,
    some_avg60: f64,
    full_avg10: f64,
}

/// The stall `/proc/pressure/memory`'s `text` reports.
#[cfg(any(target_os = "linux", test))]
fn linux_stall(text: &str) -> Result<Stall> {
    let averages = |kind: &str| -> Result<[f64; 2]> {
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix(kind)?.strip_prefix(' '))
            .with_context(|| format!("missing {kind} memory stall"))?;
        let average = |name: &str| -> Result<f64> {
            let value: f64 = line
                .split_whitespace()
                .find_map(|field| field.strip_prefix(name)?.strip_prefix('='))
                .with_context(|| format!("missing {kind} {name} memory stall"))?
                .parse()?;
            ensure!(
                (0.0..=100.0).contains(&value),
                "invalid memory stall {value}"
            );
            Ok(value)
        };
        Ok([average("avg10")?, average("avg60")?])
    };
    let [some_avg10, some_avg60] = averages("some")?;
    let [full_avg10, _] = averages("full")?;
    Ok(Stall {
        some_avg10,
        some_avg60,
        full_avg10,
    })
}

/// How `memory` and `stall`, `None` without PSI, stand against `levels`:
/// either signal past its level counts.
#[cfg(any(target_os = "linux", test))]
fn linux_pressure(memory: Memory, stall: Option<Stall>, levels: &MemoryLevels) -> MemoryPressure {
    let stalled = |average: fn(&Stall) -> f64, percent: u8| {
        stall.is_some_and(|stall| average(&stall) >= f64::from(percent))
    };
    MemoryPressure {
        critical: memory.used_at_least(levels.stop.used_percent),
        stalled: stalled(|stall| stall.full_avg10, levels.stop.stall_percent),
        warning: memory.used_at_least(levels.warning.used_percent)
            || stalled(|stall| stall.some_avg10, levels.warning.stall_percent),
        safe: !memory.used_at_least(levels.recovery.used_percent)
            && !stalled(|stall| stall.some_avg60, levels.recovery.stall_percent),
    }
}

/// macOS's native pressure level alone decides, whatever `levels` say:
/// critical stops at once, and only normal is safe.
#[cfg(target_os = "macos")]
pub fn memory(_levels: &MemoryLevels) -> Result<MemoryReading> {
    let level = native_level()?;
    Ok(MemoryReading {
        memory: native_memory()?,
        pressure: MemoryPressure {
            critical: level == 4,
            stalled: false,
            warning: level != 1,
            safe: level == 1,
        },
    })
}

/// macOS reports the percentage of memory available beside its pressure level.
#[cfg(target_os = "macos")]
fn native_memory() -> Result<Memory> {
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

/// macOS's memory pressure level: 1 normal, 2 warning, 4 critical.
#[cfg(target_os = "macos")]
fn native_level() -> Result<libc::c_int> {
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
pub fn memory(_levels: &MemoryLevels) -> Result<MemoryReading> {
    anyhow::bail!("memory monitoring is unsupported on this platform")
}

#[cfg(test)]
mod tests;
