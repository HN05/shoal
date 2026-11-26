//! Shoal's work against the user's memory limit. On Linux, once the work
//! slice exists, what its control group holds that reclaim cannot free,
//! with what it swapped out, and how long its tasks stall, together with
//! what the daemon's usage samples count of work outside it; elsewhere, and
//! before the slice exists, what those samples count for every space.
use anyhow::{Context, Result};

use super::{Memory, Stall};

/// The user's limit on Shoal's work, with what the daemon samples of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkLimit {
    pub limit: u64,
    /// What the daemon's latest usage sample counts, `None` without a
    /// current one.
    pub sampled: Option<SampledWork>,
}

/// The memory, in bytes, the daemon's latest usage sample counts for every
/// space's processes and containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampledWork {
    pub all: u64,
    /// What of it runs outside the work slice, as work does whose scope
    /// could not start.
    pub outside_slice: u64,
}

impl WorkLimit {
    /// The work's memory against the limit, and its stall where the kernel
    /// reports it.
    pub(super) fn read(self) -> Result<(Memory, Option<Stall>)> {
        let (used, stall) = self.used(slice()?)?;
        Ok((self.against(used), stall))
    }

    /// The bytes work uses, and its stall, beside `slice`, the work slice's
    /// reading where it exists: what the slice holds with what the sample
    /// counts outside it, otherwise all the sample counts. Without a current
    /// sample there is no reading, since work outside the slice may hold
    /// any amount.
    fn used(self, slice: Option<Slice>) -> Result<(u64, Option<Stall>)> {
        let sampled = self.sampled.context("no usage sample of Shoal's work")?;
        Ok(match slice {
            Some(slice) => (
                slice.used.saturating_add(sampled.outside_slice),
                slice.stall,
            ),
            None => (sampled.all, None),
        })
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
#[derive(Clone, Copy)]
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
    let used = crate::process::control_group::held(&group).context("the work slice's memory")?;
    let Some(used) = used else {
        return Ok(None);
    };
    Ok(Some(Slice {
        used,
        stall: super::read_stall(&group.join("memory.pressure"))?,
    }))
}

#[cfg(not(target_os = "linux"))]
fn slice() -> Result<Option<Slice>> {
    Ok(None)
}

#[cfg(test)]
mod tests;
