//! Wake an agent that has stayed idle as long as its mode's
//! `idle_wake_minutes`, as a backup to the wakes its updates bring: a
//! message asks it to continue or to sleep. An agent whose space is done,
//! or that sleeps, is left idle.
use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;

use super::{log, workspace::Manager};
use crate::{
    client::space_ids::SpaceId, daemon::workspace::agent_state, model::PresentSpace, time::UnixTime,
};

pub(crate) mod sleep;

/// How often Shoal looks for agents due a wake.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

pub(super) async fn run(manager: Arc<Manager>) {
    let mut woken = Woken::default();
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = manager.wake_idle_agents(&mut woken, UnixTime::now()).await {
            log!("idle wakes: {error:#}");
        }
    }
}

/// When Shoal last woke each idle agent, so one that stays idle is woken
/// again only after another period.
#[derive(Debug, Default)]
pub(crate) struct Woken(HashMap<SpaceId, UnixTime>);

impl Woken {
    /// Whether an agent of the space `id`, idle since `since`, is due a wake
    /// at `now` after `period`: it has been idle that long, and that long
    /// has passed since its last wake.
    fn due(&self, id: &SpaceId, since: UnixTime, period: Duration, now: UnixTime) -> bool {
        let from = self.0.get(id).map_or(since, |woken| since.max(*woken));
        now.duration_since(from) >= period
    }
}

impl Manager {
    /// Wake each idle agent due a wake at `now`.
    pub(crate) async fn wake_idle_agents(&self, woken: &mut Woken, now: UnixTime) -> Result<()> {
        let idle = self.store.read(|db| agent_state::idle(db)).await?;
        woken
            .0
            .retain(|id, _| idle.iter().any(|(idle, _)| idle == id));
        for (id, since) in idle {
            if let Err(error) = self.wake_if_due(&id, since, woken, now).await {
                log!("idle wake of space {id}: {error:#}");
            }
        }
        Ok(())
    }

    /// Wake the agent of the space `id`, idle since `since`, when its mode
    /// wakes it and it is due at `now`, unless the space is done or the
    /// agent sleeps.
    async fn wake_if_due(
        &self,
        id: &SpaceId,
        since: UnixTime,
        woken: &mut Woken,
        now: UnixTime,
    ) -> Result<()> {
        let space = self.space_by_id(id).await?;
        if space.done() || space.sleep().is_some() {
            return Ok(());
        }
        let Some(minutes) = self.idle_wake_minutes(&space).await? else {
            return Ok(());
        };
        if !woken.due(id, since, Duration::from_secs(minutes * 60), now) {
            return Ok(());
        }
        (self.queue_space_message(id, None, wake_message(minutes), Vec::new())).await?;
        woken.0.insert(id.to_owned(), now);
        Ok(())
    }

    /// How many minutes the agent of `space` may stay idle before Shoal
    /// wakes it, as its mode sets; none when it never does or the space has
    /// had no agent.
    pub(crate) async fn idle_wake_minutes(&self, space: &PresentSpace) -> Result<Option<u64>> {
        let Some(mode) = space.mode() else {
            return Ok(None);
        };
        Ok(self
            .present_space_settings(space)
            .await?
            .idle_wake_minutes(mode))
    }
}

/// What a wake tells an agent idle for `minutes`.
fn wake_message(minutes: u64) -> String {
    format!(
        "You have been idle for {minutes} minutes. Continue your work if any remains; otherwise run `shoal sleep`."
    )
}

#[cfg(test)]
mod tests;
