//! When putting an idle agent to sleep costs no prompt cache: a restored
//! agent's first request writes most of its cache again, so under the
//! `balanced` and `cache` schedules an agent sleeps only once its cache has
//! expired, or while the CI it waits for on its PR is expected to outlast
//! the cache. `speed` puts it to sleep at once.
use anyhow::Result;

use super::{Manager, Turn};
use crate::{client::space_ids::SpaceId, config::machine::Schedule, time::UnixTime};

/// Why putting an idle agent to sleep costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Free {
    /// Its prompt cache has expired, which recording its stop checks again,
    /// since a turn that ended meanwhile renews it.
    Expired,
    /// Its cache would expire before what it waits for arrives, or the
    /// schedule spends the cache for speed.
    Anyway,
}

impl Free {
    /// The time recording the stop must find the cache expired by, if any.
    pub(super) fn cache_expired_at(self, now: UnixTime) -> Option<UnixTime> {
        match self {
            Self::Expired => Some(now),
            Self::Anyway => None,
        }
    }
}

impl Manager {
    /// Whether putting the agent of the space `id`, idle in `turn`, to sleep
    /// at `now` costs nothing under the machine's schedule, and why. CI the
    /// space waits for that Shoal cannot estimate waits for the cache to
    /// expire.
    pub(super) async fn sleep_cost(
        &self,
        id: &SpaceId,
        turn: &Turn,
        now: UnixTime,
    ) -> Result<Option<Free>> {
        let Some(expires_at) = turn.cache_expires_at.filter(|at| *at > now) else {
            return Ok(Some(Free::Expired));
        };
        Ok(match self.config().machine.schedule {
            Schedule::Speed => Some(Free::Anyway),
            Schedule::Balanced | Schedule::Cache => {
                let left = expires_at.duration_since(now);
                let ci = self.ci_remaining(id, now).await?;
                ci.is_some_and(|ci| ci > left).then_some(Free::Anyway)
            }
        })
    }
}
