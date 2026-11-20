//! Shoal's work against the user's memory limit. On Linux, once the work
//! slice exists, what its control group holds that reclaim cannot free,
//! with what it swapped out, and how long its tasks stall; elsewhere, and
//! before the slice exists, what the daemon's usage samples count for every
//! space.
use anyhow::{Context, Result};

use super::{Memory, Stall};

/// The user's limit on Shoal's work, with what the daemon samples of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkLimit {
    /// The limit in bytes.
    pub limit: u64,
    /// The memory the daemon's latest usage samples count for every space,
    /// `None` without a current sample.
    pub sampled: Option<u64>,
}

impl WorkLimit {
    /// The work's memory against the limit, and its stall where the kernel
    /// reports it.
    pub(super) fn read(self) -> Result<(Memory, Option<Stall>)> {
        let (used, stall) = match slice()? {
            Some(slice) => (slice.used, slice.stall),
            None => {
                let sampled = self.sampled.context("no usage sample of Shoal's work")?;
                (sampled, None)
            }
        };
        Ok((self.against(used), stall))
    }

    /// `used` bytes against the limit.
    fn against(self, used: u64) -> Memory {
        Memory {
            total: self.limit,
            available: self.limit.saturating_sub(used),
        }
    }
}

/// What the work slice holds, and how long its tasks stall.
struct Slice {
    used: u64,
    stall: Option<Stall>,
}

/// The work slice's reading; `None` before it exists.
#[cfg(target_os = "linux")]
fn slice() -> Result<Option<Slice>> {
    let Some(group) = crate::service::work::control_group() else {
        return Ok(None);
    };
    let read = |name: &str| std::fs::read_to_string(group.join(name));
    let current = match read("memory.current") {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read the work slice's memory"),
    };
    let stat = read("memory.stat").context("read the work slice's memory")?;
    // Without swap accounting the file is missing.
    let swap = read("memory.swap.current").ok();
    Ok(Some(Slice {
        used: held(&current, &stat, swap.as_deref())?,
        stall: super::read_stall(&group.join("memory.pressure"))?,
    }))
}

#[cfg(not(target_os = "linux"))]
fn slice() -> Result<Option<Slice>> {
    Ok(None)
}

/// The bytes a control group holds that reclaim cannot free, with what it
/// swapped out: its `memory.current`, `current`, less the page cache of
/// files `memory.stat`'s `stat` reports, but for shared memory, which only
/// swaps, and with its `memory.swap.current`, `swap`.
#[cfg(any(target_os = "linux", test))]
fn held(current: &str, stat: &str, swap: Option<&str>) -> Result<u64> {
    let number = |text: &str| -> Result<u64> {
        (text.trim().parse()).with_context(|| format!("invalid memory reading {text:?}"))
    };
    let field = |name: &str| -> Result<u64> {
        let value = (stat.lines())
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
            .with_context(|| format!("missing {name} in memory.stat"))?;
        number(value)
    };
    let cache = field("file")?.saturating_sub(field("shmem")?);
    let swapped = swap.map_or(Ok(0), number)?;
    Ok(number(current)?
        .saturating_sub(cache)
        .saturating_add(swapped))
}

#[cfg(test)]
mod tests;
