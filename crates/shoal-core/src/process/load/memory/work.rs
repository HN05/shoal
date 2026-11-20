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
