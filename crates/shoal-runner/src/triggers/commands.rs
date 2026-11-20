//! What `shoal trigger` asks of the daemon: the triggers of the repository
//! a caller acts in and the global config's, with their runtime state, and
//! pausing, resuming and running one, as the caller's `triggers`
//! permission allows.
use std::sync::Arc;

use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::{
    config::permissions::PermissionKey,
    protocol::ItemContext,
    triggers::{Fired, RunOrigin, TriggerAction, TriggerStatus, WebhookRequest},
};

use super::{Configured, firing::Firing, state};
use crate::{Runner, daemon::Daemon, time::UnixTime};

impl Runner {
    /// The triggers of the repository `context` acts in, after the global
    /// config's, each with its runtime state.
    pub async fn list_triggers(
        &self,
        daemon: &dyn Daemon,
        context: Option<ItemContext>,
    ) -> Result<Vec<TriggerStatus>> {
        let repository = daemon.context_repository(context).await?;
        let now = UnixTime::now();
        let mut statuses = Vec::new();
        for trigger in self.triggers_of(daemon, repository.as_ref()).await? {
            statuses.push(self.trigger_status(&trigger, now).await?);
        }
        Ok(statuses)
    }

    /// Pause or resume the trigger `name` of the repository `context` acts
    /// in, else of the global config, as `caller`'s `triggers` permission
    /// allows.
    pub async fn pause_trigger(
        &self,
        daemon: &dyn Daemon,
        context: Option<ItemContext>,
        name: &str,
        paused: bool,
        caller: Option<&Caller>,
    ) -> Result<TriggerStatus> {
        let action = match paused {
            true => TriggerAction::Pause,
            false => TriggerAction::Resume,
        };
        let trigger = (self.acted_on_trigger(daemon, context, name, action, caller)).await?;
        let (scope, name, now) = (trigger.scope(), trigger.name.clone(), UnixTime::now());
        self.store
            .write(move |tx| state::set_paused(tx, &scope, &name, paused, now))
            .await?;
        self.triggers.wake();
        self.trigger_status(&trigger, now).await
    }

    /// Fire the trigger `name` of the repository `context` acts in, else of
    /// the global config, now, a webhook trigger with `request`, as
    /// `caller`'s `triggers` permission allows.
    pub async fn run_trigger(
        &self,
        daemon: &Arc<dyn Daemon>,
        context: Option<ItemContext>,
        name: &str,
        request: Option<WebhookRequest>,
        caller: Option<&Caller>,
    ) -> Result<Fired> {
        let action = TriggerAction::Run;
        let trigger = (self.acted_on_trigger(&**daemon, context, name, action, caller)).await?;
        let _firing = self.triggers.firing.lock().await;
        let now = UnixTime::now();
        let state = self.trigger_state(&trigger, now).await?;
        let input = (trigger.trigger.source).manual_input(
            &trigger.name,
            now,
            state.previous(),
            request.as_ref(),
        )?;
        let firing = Firing {
            origin: RunOrigin::Manual,
            input,
            at: now,
        };
        self.fire_trigger(daemon, &trigger, firing).await
    }

    /// The trigger `name` `action` acts on, once `caller`'s `triggers`
    /// permission allows it.
    async fn acted_on_trigger(
        &self,
        daemon: &dyn Daemon,
        context: Option<ItemContext>,
        name: &str,
        action: TriggerAction,
        caller: Option<&Caller>,
    ) -> Result<Configured> {
        let repository = daemon.context_repository(context).await?;
        let trigger = (self.named_trigger(daemon, repository.as_ref(), name)).await?;
        let described = format!("{action} the {}", trigger.label());
        let acts_in = trigger.repository.clone().or(repository);
        (daemon.authorize(caller, PermissionKey::Triggers, described, acts_in)).await?;
        Ok(trigger)
    }

    /// `trigger` with its runtime state as of `now`.
    async fn trigger_status(&self, trigger: &Configured, now: UnixTime) -> Result<TriggerStatus> {
        let state = self.trigger_state(trigger, now).await?;
        let (scope, name) = (trigger.scope(), trigger.name.clone());
        let last_run = self
            .store
            .read(move |tx| state::last_run(tx, &scope, &name))
            .await?;
        let source = &trigger.trigger.source;
        let next_at = (!state.paused)
            .then(|| source.next(state.counted_from, state.last_fired_at))
            .flatten();
        Ok(TriggerStatus {
            name: trigger.name.clone(),
            repository: trigger.repository.clone(),
            source: trigger.trigger.source.kind(),
            paused: state.paused,
            next_at,
            last_run,
        })
    }
}
