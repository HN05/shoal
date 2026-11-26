#[cfg(any(target_os = "linux", test))]
use anyhow::Context;
use anyhow::{Result, ensure};

/// Cumulative CPU ticks across every core: those used at normal priority,
/// those niced and the total, in which I/O wait counts as idle. Linux guest
/// ticks are already included in user/nice and must not be counted twice.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CpuTicks {
    pub used: u64,
    pub niced: u64,
    pub total: u64,
}

impl CpuTicks {
    /// The share of ticks since `previous` used at normal priority, with
    /// niced time idle: work at low priority, such as CI jobs, takes only
    /// cores nothing else wants.
    pub fn used_percent_since(self, previous: Self) -> Option<f64> {
        self.percent_since(previous, |ticks| Some(ticks.used))
    }

    /// The share of ticks since `previous` that ran any work, niced time
    /// included, which work at low priority competes for with each other.
    pub fn busy_percent_since(self, previous: Self) -> Option<f64> {
        self.percent_since(previous, |ticks| ticks.used.checked_add(ticks.niced))
    }

    fn percent_since(self, previous: Self, part: impl Fn(Self) -> Option<u64>) -> Option<f64> {
        let total = self.total.checked_sub(previous.total)?;
        let part = part(self)?.checked_sub(part(previous)?)?;
        (total > 0 && part <= total).then(|| part as f64 * 100.0 / total as f64)
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
    let niced = values[1];
    // idle and iowait
    let idle = [3, 4]
        .into_iter()
        .try_fold(0u64, |sum, index| {
            sum.checked_add(values.get(index).copied().unwrap_or_default())
        })
        .context("idle ticks overflow")?;
    Ok(CpuTicks {
        used: total - idle - niced,
        niced,
        total,
    })
}

#[cfg(target_os = "macos")]
pub fn cpu_ticks() -> Result<CpuTicks> {
    // Retain one host port for the process lifetime instead of acquiring a new
    // Mach send right on every sample.
    pub(crate) static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    #[expect(
        deprecated,
        reason = "libc keeps the native ABI; a new dependency for one call is not worth it"
    )]
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

/// The used, niced and total ticks of Mach's per-state `ticks`.
#[cfg(any(target_os = "macos", test))]
fn mach_cpu(ticks: [u64; 4]) -> CpuTicks {
    let total = ticks.iter().sum();
    CpuTicks {
        total,
        niced: ticks[MACH_NICE],
        used: total - ticks[MACH_IDLE] - ticks[MACH_NICE],
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn cpu_ticks() -> Result<CpuTicks> {
    anyhow::bail!("CPU monitoring is unsupported on this platform")
}

#[cfg(test)]
mod tests;
