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
pub(crate) fn memory_level() -> Result<libc::c_int> {
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

#[cfg(test)]
mod tests;
