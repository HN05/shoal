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
    Ok(level == 4)
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
