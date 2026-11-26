use anyhow::{Context, Result, ensure};

mod work;

pub use work::{SampledWork, WorkLimit};

/// Memory in bytes: all of it, and what new work can use without pushing
/// other memory out. On Linux the machine's both count swap on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
}

impl Memory {
    /// The share of the memory in use, in percent.
    pub fn used_percent(self) -> f64 {
        let used = self.total.saturating_sub(self.available);
        used as f64 * 100.0 / self.total.max(1) as f64
    }

    /// Whether at least `percent` of the memory is in use.
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

impl MemoryPressure {
    /// The pressure of two readings together: past a level where either
    /// is, and safe only where both are.
    fn or(self, other: Self) -> Self {
        Self {
            critical: self.critical || other.critical,
            stalled: self.stalled || other.stalled,
            warning: self.warning || other.warning,
            safe: self.safe && other.safe,
        }
    }
}

/// One reading of the machine's memory and its pressure, with Shoal's
/// work's against the user's limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryReading {
    pub memory: Memory,
    /// The machine's stall; none without pressure stall information.
    pub stall: Option<Stall>,
    /// Shoal's work against the user's limit; none without a limit.
    pub work: Option<WorkMemory>,
    /// The machine's and the work's together.
    pub pressure: MemoryPressure,
}

/// Shoal's work's memory against the user's limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkMemory {
    /// The limit as `total` and what the work leaves of it, with the work
    /// slice's stall where the kernel reports it.
    Read {
        memory: Memory,
        stall: Option<Stall>,
    },
    /// The work could not be read, as without a current usage sample.
    Unread,
}

/// Read the machine's memory and its pressure, and with `work` Shoal's
/// work's against its limit, which counts as the machine's own: pressure is
/// past a level where either is. Work that cannot be read leaves the
/// machine's pressure to act on, but nothing safe to recover on.
pub fn memory(levels: &MemoryLevels, work: Option<WorkLimit>) -> Result<MemoryReading> {
    let machine = machine(levels)?;
    let Some(work) = work else {
        return Ok(machine);
    };
    Ok(match work.read() {
        Ok((used, stall)) => MemoryReading {
            work: Some(WorkMemory::Read {
                memory: used,
                stall,
            }),
            pressure: machine.pressure.or(pressure(used, stall, levels)),
            ..machine
        },
        Err(_) => MemoryReading {
            work: Some(WorkMemory::Unread),
            pressure: MemoryPressure {
                safe: false,
                ..machine.pressure
            },
            ..machine
        },
    })
}

/// Read memory in use with swap on disk, and the stall where the kernel
/// reports it.
#[cfg(target_os = "linux")]
fn machine(levels: &MemoryLevels) -> Result<MemoryReading> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read memory availability")?;
    let memory = linux_memory(&text, &read_swaps()?)?;
    let stall = read_stall("/proc/pressure/memory".as_ref())?;
    Ok(MemoryReading {
        memory,
        stall,
        work: None,
        pressure: pressure(memory, stall, levels),
    })
}

/// The machine's RAM in bytes, without swap.
#[cfg(target_os = "linux")]
pub fn ram() -> Result<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read memory availability")?;
    meminfo_bytes(&text, "MemTotal:")
}

/// The bytes the line `name` of `/proc/meminfo`'s `text` reports.
#[cfg(any(target_os = "linux", test))]
fn meminfo_bytes(text: &str, name: &str) -> Result<u64> {
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
}

/// The memory stall the kernel reports in the pressure file `path`, `None`
/// without PSI: a kernel built without it, or booted with `psi=0`, has no
/// pressure file.
#[cfg(target_os = "linux")]
fn read_stall(path: &std::path::Path) -> Result<Option<Stall>> {
    match std::fs::read_to_string(path) {
        Ok(text) => linux_stall(&text).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The swap devices and files `/proc/swaps` lists, empty for a kernel built
/// without swap, which has no such file.
#[cfg(target_os = "linux")]
fn read_swaps() -> Result<String> {
    match std::fs::read_to_string("/proc/swaps") {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).context("read /proc/swaps"),
    }
}

/// The total and available memory, RAM and swap on disk together, that
/// `/proc/meminfo`'s `meminfo` and `/proc/swaps`'s `swaps` report.
#[cfg(any(target_os = "linux", test))]
fn linux_memory(meminfo: &str, swaps: &str) -> Result<Memory> {
    let read = |name: &str| meminfo_bytes(meminfo, name);
    let total = read("MemTotal:")?;
    let available = read("MemAvailable:")?;
    ensure!(
        total > 0 && available <= total,
        "invalid memory availability"
    );
    let swap = disk_swap(swaps)?;
    Ok(Memory {
        total: total
            .checked_add(swap.total)
            .context("memory reading overflow")?,
        available: available.saturating_add(swap.available),
    })
}

/// The swap on disk that `/proc/swaps`'s `text` lists, as [`Memory`]. A zram
/// device keeps what it swaps in RAM, compressed, so it counts as neither:
/// RAM already counts what it holds, and a full zram device comes only long
/// after the machine has stopped responding.
#[cfg(any(target_os = "linux", test))]
fn disk_swap(text: &str) -> Result<Memory> {
    let mut swap = Memory {
        total: 0,
        available: 0,
    };
    // The first line names the columns.
    for line in text.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(_kind), Some(size), Some(used)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            anyhow::bail!("invalid swap line {line:?}");
        };
        let kib = |value: &str| -> Result<u64> {
            let value: u64 = value.parse()?;
            value.checked_mul(1024).context("swap reading overflow")
        };
        let (size, used) = (kib(size)?, kib(used)?);
        ensure!(used <= size, "invalid swap use of {name}");
        if is_zram(name) {
            continue;
        }
        swap.total = swap
            .total
            .checked_add(size)
            .context("swap reading overflow")?;
        swap.available += size - used;
    }
    Ok(swap)
}

/// Whether the swap `name` is a zram device, `/dev/zram0` and on.
#[cfg(any(target_os = "linux", test))]
fn is_zram(name: &str) -> bool {
    name.strip_prefix("/dev/zram")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// The share of time, in percent, that tasks stalled waiting for memory:
/// some of them over ten seconds and over a minute, and all of them over ten
/// seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stall {
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub full_avg10: f64,
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
fn pressure(memory: Memory, stall: Option<Stall>, levels: &MemoryLevels) -> MemoryPressure {
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
fn machine(_levels: &MemoryLevels) -> Result<MemoryReading> {
    let level = native_level()?;
    Ok(MemoryReading {
        memory: native_memory()?,
        stall: None,
        work: None,
        pressure: MemoryPressure {
            critical: level == 4,
            stalled: false,
            warning: level != 1,
            safe: level == 1,
        },
    })
}

/// The machine's RAM in bytes.
#[cfg(target_os = "macos")]
pub fn ram() -> Result<u64> {
    sysctl(c"hw.memsize")
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
pub fn ram() -> Result<u64> {
    anyhow::bail!("memory monitoring is unsupported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn machine(_levels: &MemoryLevels) -> Result<MemoryReading> {
    anyhow::bail!("memory monitoring is unsupported on this platform")
}

#[cfg(test)]
mod tests;
